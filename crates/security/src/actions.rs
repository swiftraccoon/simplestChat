//! The review of the actions a change pins anew.
//!
//! Every action a workflow uses is pinned to a commit. A pin that the
//! snapshot holds and its base does not is reviewed before it is trusted:
//! the licence of the repository at that commit must be allowed, and no
//! advisory may affect a release that the commit is.
//!
//! A local action needs no pin, but the review must have read it: where a
//! workflow of the snapshot uses a local path, that path names a workflow
//! or the description of an action among the files read, or it is refused.
//! An action below `vendor/`, behind a link or made while a job runs is
//! therefore no action a workflow may use unseen.

use crate::{
    Fault, Result,
    context::{Context, Failure, Outcome, Run},
    escaped,
    fast::Programs,
    files,
    licenses::{allowed_licenses, splitlines},
    record::{self, quoted},
    require, scope,
    spdx::{Allowed, permits},
};
use serde::de::{Deserialize, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use serde_saphyr::{DuplicateKeyPolicy, MergeKeyPolicy};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    path::Path,
    time::{Duration, Instant},
};

/// The most bytes of one workflow or of one answer.
const MAX_BYTES: u64 = 4 * 1024 * 1024;
/// The most mappings and lists one workflow holds.
const MAX_NODES: usize = 20_000;
/// The most workflows and descriptions of actions one tree holds.
const MAX_WORKFLOWS: usize = 500;
/// How many advisories one page of the API holds.
const PAGE: usize = 100;
/// The most releases one commit is asked about.
const MAX_VERSIONS: usize = 16;
/// The most new pins of one change.
const MAX_CHANGED: usize = 64;
/// How long one command of the review has.
const COMMAND_TIME: Duration = Duration::from_secs(45);
/// How long the whole review has.
const REVIEW_TIME: Duration = Duration::from_secs(600);
/// How many bytes of complaints one command may make.
const COMPLAINTS: u64 = 65536;
/// Where GitHub's public metadata is asked for.
const API: &str = "https://api.github.com";
/// The receipt, in the output directory.
const RECEIPT: &str = "actions-dependencies.json";
/// A workflow that is no YAML, or YAML this reader does not take.
const YAML: Fault = Fault("actions_workflow_yaml");
/// A command of the review that did not end well.
const FAILED: Fault = Fault("actions_metadata_command_failed");
/// An answer in a form the review does not read.
const FORMAT: Fault = Fault("actions_metadata_format");

/// What a pin names: a repository, the path of the action below it or
/// none, and a commit.
struct Pin<'a> {
    repository: &'a str,
    path: Option<&'a str>,
    commit: &'a str,
}

/// The parts of the pin `literal`: an owner and a repository, a path below
/// it or none, and the full name of a commit. Anything else is no pin.
fn pin(literal: &str) -> Option<Pin<'_>> {
    let (action, commit) = literal.split_once('@')?;
    let named = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-');
    let part = |part: &str| !part.is_empty() && part.bytes().all(named);
    let mut parts = action.splitn(3, '/');
    let (owner, name, path) = (parts.next()?, parts.next()?, parts.next());
    let below =
        |path: &str| !path.is_empty() && path.bytes().all(|byte| named(byte) || byte == b'/');
    let valid = part(owner) && part(name) && path.is_none_or(below) && crate::commit(commit);
    valid.then(|| Pin {
        repository: &action[..owner.len() + 1 + name.len()],
        path,
        commit,
    })
}

/// The key of a mapping: the one that names an action, or any other
/// scalar. A key that is a list or a mapping is no key of a workflow.
enum Key {
    Uses,
    Other,
}

impl<'de> Deserialize<'de> for Key {
    fn deserialize<D: Deserializer<'de>>(source: D) -> std::result::Result<Self, D::Error> {
        struct Read;
        impl<'de> Visitor<'de> for Read {
            type Value = Key;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a scalar key")
            }

            fn visit_str<E>(self, key: &str) -> std::result::Result<Key, E> {
                Ok(if key == "uses" { Key::Uses } else { Key::Other })
            }

            fn visit_bool<E>(self, _: bool) -> std::result::Result<Key, E> {
                Ok(Key::Other)
            }

            fn visit_i64<E>(self, _: i64) -> std::result::Result<Key, E> {
                Ok(Key::Other)
            }

            fn visit_u64<E>(self, _: u64) -> std::result::Result<Key, E> {
                Ok(Key::Other)
            }

            fn visit_f64<E>(self, _: f64) -> std::result::Result<Key, E> {
                Ok(Key::Other)
            }

            fn visit_unit<E>(self) -> std::result::Result<Key, E> {
                Ok(Key::Other)
            }
        }
        source.deserialize_any(Read)
    }
}

/// What a workflow says it uses: text, or anything else.
enum Literal {
    Text(String),
    Other,
}

impl<'de> Deserialize<'de> for Literal {
    fn deserialize<D: Deserializer<'de>>(source: D) -> std::result::Result<Self, D::Error> {
        struct Read;
        impl<'de> Visitor<'de> for Read {
            type Value = Literal;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("any value")
            }

            fn visit_str<E>(self, text: &str) -> std::result::Result<Literal, E> {
                Ok(Literal::Text(text.to_owned()))
            }

            fn visit_bool<E>(self, _: bool) -> std::result::Result<Literal, E> {
                Ok(Literal::Other)
            }

            fn visit_i64<E>(self, _: i64) -> std::result::Result<Literal, E> {
                Ok(Literal::Other)
            }

            fn visit_u64<E>(self, _: u64) -> std::result::Result<Literal, E> {
                Ok(Literal::Other)
            }

            fn visit_f64<E>(self, _: f64) -> std::result::Result<Literal, E> {
                Ok(Literal::Other)
            }

            fn visit_unit<E>(self) -> std::result::Result<Literal, E> {
                Ok(Literal::Other)
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut items: A,
            ) -> std::result::Result<Literal, A::Error> {
                while items.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Literal::Other)
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut pairs: A,
            ) -> std::result::Result<Literal, A::Error> {
                while pairs.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Literal::Other)
            }
        }
        source.deserialize_any(Read)
    }
}

/// What one value of a workflow holds: the pins and the local paths below
/// it, how many mappings and lists it is made of, and the first fault met
/// in it.
#[derive(Default)]
struct Walk {
    pins: BTreeSet<String>,
    locals: BTreeSet<String>,
    nodes: usize,
    fault: Option<Fault>,
}

impl Walk {
    fn join(&mut self, inner: Self) {
        self.pins.extend(inner.pins);
        self.locals.extend(inner.locals);
        self.nodes += inner.nodes;
        self.fault = self.fault.or(inner.fault);
    }

    /// Judge what a mapping says it uses. A local action needs no pin: its
    /// path is kept as the tree spells it, without the slashes that end it.
    fn uses(&mut self, literal: Literal) {
        let fault = match literal {
            Literal::Other => Some(Fault("actions_use_not_literal")),
            Literal::Text(text) if text.starts_with("./") => {
                let path = text[2..].trim_end_matches('/');
                self.locals.insert(path.to_owned());
                None
            }
            Literal::Text(text) if pin(&text).is_some() => {
                self.pins.insert(text);
                None
            }
            Literal::Text(_) => Some(Fault("actions_unpinned_or_unsupported_use")),
        };
        self.fault = self.fault.or(fault);
    }
}

impl<'de> Deserialize<'de> for Walk {
    fn deserialize<D: Deserializer<'de>>(source: D) -> std::result::Result<Self, D::Error> {
        struct Read;
        impl<'de> Visitor<'de> for Read {
            type Value = Walk;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("any value")
            }

            fn visit_str<E>(self, _: &str) -> std::result::Result<Walk, E> {
                Ok(Walk::default())
            }

            fn visit_bool<E>(self, _: bool) -> std::result::Result<Walk, E> {
                Ok(Walk::default())
            }

            fn visit_i64<E>(self, _: i64) -> std::result::Result<Walk, E> {
                Ok(Walk::default())
            }

            fn visit_u64<E>(self, _: u64) -> std::result::Result<Walk, E> {
                Ok(Walk::default())
            }

            fn visit_f64<E>(self, _: f64) -> std::result::Result<Walk, E> {
                Ok(Walk::default())
            }

            fn visit_unit<E>(self) -> std::result::Result<Walk, E> {
                Ok(Walk::default())
            }

            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut items: A,
            ) -> std::result::Result<Walk, A::Error> {
                let mut walk = Walk {
                    nodes: 1,
                    ..Walk::default()
                };
                while let Some(inner) = items.next_element::<Walk>()? {
                    walk.join(inner);
                }
                Ok(walk)
            }

            fn visit_map<A: MapAccess<'de>>(
                self,
                mut pairs: A,
            ) -> std::result::Result<Walk, A::Error> {
                let mut walk = Walk {
                    nodes: 1,
                    ..Walk::default()
                };
                while let Some(key) = pairs.next_key::<Key>()? {
                    match key {
                        Key::Uses => walk.uses(pairs.next_value::<Literal>()?),
                        Key::Other => walk.join(pairs.next_value::<Walk>()?),
                    }
                }
                Ok(walk)
            }
        }
        source.deserialize_any(Read)
    }
}

/// Every action that the workflow `data` pins, wherever a mapping of it
/// says what it uses: in a step, in a job that calls another workflow, in
/// a composite action. A use that is no text, or text that is neither a
/// local path nor a pin, is refused. Every use the workflow writes is
/// judged, also one that a later key of the same name replaces, and of
/// several faults the first in the order of the workflow is answered.
///
/// A workflow is one UTF-8 document of at most 4 MiB and 20,000 mappings
/// and lists, nested at most 64 deep. Aliases are followed 1,024 times at
/// most, and a tag of an application's own is refused.
pub fn pins(data: &[u8]) -> Result<BTreeSet<String>> {
    Ok(walked(data)?.pins)
}

/// The stack on which a workflow is read.
const STACK: usize = 16 * 1024 * 1024;

/// The workflow `data` as its one YAML document, within the bounds that
/// [`pins`] says.
fn document(data: &[u8]) -> Option<Walk> {
    let options = serde_saphyr::options! {
        duplicate_keys: DuplicateKeyPolicy::LastWins,
        merge_keys: MergeKeyPolicy::Merge,
        strict_booleans: false,
        reject_unsupported_tags: true,
        budget: serde_saphyr::budget! {
            max_reader_input_bytes: Some(MAX_BYTES as usize),
            max_events: 2_000_000,
            max_nodes: 1_000_000,
            max_depth: 64,
            flow_nesting_limit: 64,
            max_aliases: 1_024,
            max_anchors: 1_024,
            max_recorded_anchor_events: 100_000,
            max_recorded_anchor_bytes: 4 * 1024 * 1024,
            max_total_scalar_bytes: 4 * 1024 * 1024,
            max_merge_keys: 1_024,
            max_documents: 1,
            max_inclusion_depth: 0,
            // The bounds above say how far aliases are followed: no ratio of
            // aliases to anchors decides it as well.
            enforce_alias_anchor_ratio: false,
        },
        alias_limits: serde_saphyr::alias_limits! {
            max_total_replayed_events: 100_000,
            max_replay_stack_depth: 32,
            max_alias_expansions_per_anchor: 1_024,
        },
    };
    serde_saphyr::from_slice_with_options::<Walk>(data, options).ok()
}

/// What the workflow `data` uses, read as [`pins`] says: the actions it
/// pins and the local paths it names.
fn walked(data: &[u8]) -> Result<Walk> {
    require(data.len() as u64 <= MAX_BYTES, "actions_workflow_size")?;
    // The reader descends once for every level of nesting, and in a build
    // that is not optimised its frames are large: 64 levels need most of
    // the stack a thread gets by default. It reads on a stack of its own,
    // so that it does not depend on what whoever asks has left.
    let walk = std::thread::scope(|scope| {
        let reader = std::thread::Builder::new().stack_size(STACK);
        reader
            .spawn_scoped(scope, || document(data))
            .ok()?
            .join()
            .ok()?
    });
    let walk = walk.ok_or(YAML)?;
    require(walk.nodes <= MAX_NODES, "actions_workflow_complexity")?;
    match walk.fault {
        Some(fault) => Err(fault),
        None => Ok(walk),
    }
}

/// Whether the files `read` hold what the local path `path` names: that
/// file itself, or the description of the action in that directory.
fn described(path: &str, read: &BTreeSet<String>) -> bool {
    let within = |file: &str| match path {
        "" => file.to_owned(),
        path => format!("{path}/{file}"),
    };
    read.contains(path)
        || read.contains(&within("action.yml"))
        || read.contains(&within("action.yaml"))
}

/// Whether `tag` names a full version: three numbers, with a `v` before
/// them or none, and the labels of a pre-release and of a build after them
/// or none.
fn version(tag: &str) -> bool {
    let numbered = tag.strip_prefix('v').unwrap_or(tag);
    let digits = |text: &str| text.bytes().take_while(u8::is_ascii_digit).count();
    let mut rest = numbered;
    for part in 0..3 {
        let length = digits(rest);
        let pointed = part == 2 || rest[length..].starts_with('.');
        if length == 0 || !pointed {
            return false;
        }
        rest = &rest[length + usize::from(part < 2)..];
    }
    let labelled = |label: &str| {
        let lettered = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-');
        !label.is_empty() && label.bytes().all(lettered)
    };
    let (before, build) = match rest.split_once('+') {
        Some((before, build)) => (before, Some(build)),
        None => (rest, None),
    };
    let released = before.is_empty() || before.strip_prefix('-').is_some_and(labelled);
    released && build.is_none_or(labelled)
}

/// Whether the file `name` of a tree is read: a YAML file of the
/// repository's settings, or the description of an action anywhere.
fn read(name: &str, include_vendor: bool) -> bool {
    let described = name.rsplit('/').next();
    let action = described.is_some_and(|base| matches!(base, "action.yml" | "action.yaml"));
    let settings =
        name.starts_with(".github/") && (name.ends_with(".yml") || name.ends_with(".yaml"));
    scope::selected(name, include_vendor) && (settings || action)
}

/// The names of the files and of the links to files below `directory`,
/// each from `root`. A directory that is a link is not entered, and a name
/// that is no UTF-8 names no file of the snapshot afterwards.
fn listed(root: &Path, directory: &Path, names: &mut Vec<String>) -> Result<()> {
    let entries = std::fs::read_dir(directory).map_err(|error| files::unusable(&error))?;
    for entry in entries {
        let path = entry.map_err(|error| files::unusable(&error))?.path();
        let kind = path
            .symlink_metadata()
            .map_err(|error| files::unusable(&error))?;
        if kind.is_dir() {
            listed(root, &path, names)?;
        } else if path.is_file() {
            let below = path.strip_prefix(root).unwrap_or(&path);
            names.push(below.to_string_lossy().into_owned());
        }
    }
    Ok(())
}

/// One reviewed pin, as the receipt holds it.
struct Reviewed<'a> {
    pin: &'a str,
    license: String,
    versions: Vec<String>,
}

/// The receipt of the pins that were reviewed, in the one spelling the
/// reference gives it.
fn receipt(reviewed: &[Reviewed<'_>]) -> String {
    let pins: Vec<String> = reviewed
        .iter()
        .map(|record| {
            let versions: Vec<String> = record
                .versions
                .iter()
                .map(|version| quoted(version))
                .collect();
            format!(
                "{{\"pin\": {}, \"license\": {}, \"versions\": [{}], \"passed\": true}}",
                quoted(record.pin),
                quoted(&record.license),
                versions.join(", ")
            )
        })
        .collect();
    format!("{{\"passed\": true, \"pins\": [{}]}}\n", pins.join(", "))
}

/// How long a command may take when `left` is left of the review's time:
/// its own time at most, and none when the review's time is over.
fn patience(left: Duration) -> Option<Duration> {
    (!left.is_zero()).then(|| left.min(COMMAND_TIME))
}

/// The commands of one review: where they run, which programs they are and
/// when the review must have ended.
struct Asking<'a> {
    context: &'a Context,
    programs: &'a dyn Programs,
    ends: Instant,
}

impl Asking<'_> {
    /// What `argv` prints when it ends with the status 0. Nothing it says
    /// is kept, and no command begins once the review's time is over.
    fn command(&self, argv: &[&OsStr]) -> Outcome<Vec<u8>> {
        let left = self.ends.saturating_duration_since(Instant::now());
        let within = patience(left).ok_or(Fault("command_timed_out"))?;
        let how = Run {
            within,
            ..Run::default()
        };
        let (status, printed) = self.context.ask(argv, &how, (MAX_BYTES, COMPLAINTS))?;
        require(status == 0, FAILED.0)?;
        Ok(printed)
    }

    /// What git prints for `arguments` when it ends with the status 0.
    fn git(&self, arguments: &[&str]) -> Outcome<Vec<u8>> {
        let git = self.programs.prerequisite("git")?;
        let mut argv = vec![git.as_os_str()];
        argv.extend(arguments.iter().map(OsStr::new));
        self.command(&argv)
    }

    /// What GitHub's public API answers at `path` for `parameters`: asked
    /// without credentials, and never followed anywhere else.
    fn api(&self, path: &str, parameters: &[(&str, &str)]) -> Outcome<Value> {
        let query: Vec<String> = parameters
            .iter()
            .map(|(name, value)| format!("{}={}", escaped(name, true), escaped(value, true)))
            .collect();
        let url = format!("{API}{path}?{}", query.join("&"));
        let curl = self.programs.prerequisite("curl")?;
        let size = MAX_BYTES.to_string();
        let argv = [
            curl.as_os_str(),
            OsStr::new("--disable"),
            OsStr::new("--fail"),
            OsStr::new("--silent"),
            OsStr::new("--show-error"),
            OsStr::new("--proto"),
            OsStr::new("=https"),
            OsStr::new("--tlsv1.2"),
            OsStr::new("--connect-timeout"),
            OsStr::new("10"),
            OsStr::new("--max-time"),
            OsStr::new("30"),
            OsStr::new("--max-filesize"),
            OsStr::new(&size),
            OsStr::new("--header"),
            OsStr::new("Accept: application/vnd.github+json"),
            OsStr::new("--header"),
            OsStr::new("X-GitHub-Api-Version: 2022-11-28"),
            OsStr::new("--header"),
            OsStr::new("User-Agent: simplestChat-local-ci"),
            OsStr::new(&url),
        ];
        let printed = self.command(&argv)?;
        Ok(record::strict(&printed).map_err(|_| FORMAT)?)
    }

    /// Whether an advisory that was not withdrawn affects `package`, at
    /// `version` or at any: reviewed advisories and reports of malware, at
    /// every severity. A full page may not be all there is and is refused.
    fn advisories(&self, package: &str, version: Option<&str>) -> Outcome<bool> {
        let affects = match version {
            Some(version) => format!("{package}@{version}"),
            None => package.to_owned(),
        };
        let page = PAGE.to_string();
        let mut found = false;
        for kind in ["reviewed", "malware"] {
            let parameters = [
                ("ecosystem", "actions"),
                ("affects", affects.as_str()),
                ("type", kind),
                ("is_withdrawn", "false"),
                ("per_page", page.as_str()),
            ];
            let answer = self.api("/advisories", &parameters)?;
            let listed = answer.as_array().ok_or(FORMAT)?;
            require(listed.len() < PAGE, "actions_advisory_page_incomplete")?;
            for advisory in listed {
                let advisory = advisory.as_object().ok_or(FORMAT)?;
                let named = advisory.get("ghsa_id").is_some_and(Value::is_string);
                require(named, "actions_advisory_schema")?;
                let current = advisory.get("withdrawn_at").is_none_or(Value::is_null);
                require(current, "actions_withdrawn_filter_failed")?;
                found = true;
            }
        }
        Ok(found)
    }

    /// The versions that `revision` is released as in `repository`: its
    /// tags that name a full version, a tag object counted by the commit
    /// it names.
    fn releases(&self, repository: &str, revision: &str) -> Outcome<Vec<String>> {
        const IDENTITY: Fault = Fault("actions_tag_identity");
        let remote = format!("https://github.com/{repository}.git");
        let listing = self.git(&["ls-remote", "--tags", &remote])?;
        let listing = String::from_utf8(listing).map_err(|_| FORMAT)?;
        let mut tags = BTreeMap::new();
        let mut peeled = BTreeMap::new();
        for line in splitlines(&listing) {
            let (commit, name) = line.split_once('\t').ok_or(IDENTITY)?;
            require(crate::commit(commit), IDENTITY.0)?;
            let tag = name
                .strip_prefix("refs/tags/")
                .ok_or(Fault("actions_tag_ref"))?;
            match tag.strip_suffix("^{}") {
                Some(tag) => peeled.insert(tag, commit),
                None => tags.insert(tag, commit),
            };
        }
        tags.extend(peeled);
        let released = tags
            .iter()
            .filter(|(tag, commit)| **commit == revision && version(tag));
        let versions: BTreeSet<&str> = released
            .map(|(tag, _)| tag.strip_prefix('v').unwrap_or(tag))
            .collect();
        require(
            !versions.is_empty() && versions.len() <= MAX_VERSIONS,
            "actions_advisory_version_unresolved",
        )?;
        Ok(versions.into_iter().map(str::to_owned).collect())
    }

    /// Review one pin: the licence of its repository at that commit is
    /// allowed, and no advisory affects a release that the commit is. An
    /// action below a path is asked about under both of its names.
    fn review<'a>(&self, text: &'a str, allowed: &Allowed) -> Outcome<Reviewed<'a>> {
        let pin = pin(text).ok_or(Fault("actions_pin_identity"))?;
        let repository = pin.repository.to_ascii_lowercase();
        let below = pin.path.map(|path| format!("{repository}/{path}"));
        let metadata = self.api(
            &format!("/repos/{repository}/license"),
            &[("ref", pin.commit)],
        )?;
        let license = metadata
            .get("license")
            .and_then(|license| license.get("spdx_id"));
        let license = license.and_then(Value::as_str).ok_or(FORMAT)?;
        require(permits(license, allowed)?, "actions_license_blocked")?;
        let mut versions = Vec::new();
        let packages = BTreeSet::from([
            below.unwrap_or_else(|| repository.clone()),
            repository.clone(),
        ]);
        for package in &packages {
            if !self.advisories(package, None)? {
                continue;
            }
            if versions.is_empty() {
                versions = self.releases(&repository, pin.commit)?;
            }
            for version in &versions {
                let affected = self.advisories(package, Some(version))?;
                require(!affected, "actions_advisory_blocked")?;
            }
        }
        Ok(Reviewed {
            pin: text,
            license: license.to_owned(),
            versions,
        })
    }

    /// The pins of the workflows and actions of the snapshot, or of the
    /// tree of the commit `base` when there is no snapshot. What a workflow
    /// of the snapshot uses locally must be among the files read; the base
    /// is only what the snapshot is compared with, and is not held to that.
    fn inventory(
        &self,
        snapshot: Option<&Path>,
        base: &str,
        include_vendor: bool,
    ) -> Outcome<BTreeSet<String>> {
        let mut names = Vec::new();
        match snapshot {
            Some(root) => listed(root, root, &mut names)?,
            None => {
                let listing = self.git(&["ls-tree", "-rz", "--name-only", base])?;
                let listing = String::from_utf8(listing).map_err(|_| FORMAT)?;
                names.extend(listing.split('\0').map(str::to_owned));
            }
        }
        let read: BTreeSet<String> = names
            .into_iter()
            .filter(|name| read(name, include_vendor))
            .collect();
        require(read.len() <= MAX_WORKFLOWS, "actions_workflow_count")?;
        let mut found = BTreeSet::new();
        let mut locals = BTreeSet::new();
        for name in &read {
            let data = match snapshot {
                Some(root) => {
                    let path = root.join(name);
                    let within = path.canonicalize().ok().zip(root.canonicalize().ok());
                    let inside = within.is_some_and(|(path, root)| path.starts_with(root));
                    require(inside, "actions_source_outside_snapshot")?;
                    files::bounded(&path, MAX_BYTES, false)?
                }
                None => self.git(&["show", &format!("{base}:{name}")])?,
            };
            let walk = walked(&data)?;
            found.extend(walk.pins);
            locals.extend(walk.locals);
        }
        let seen = snapshot.is_none() || locals.iter().all(|path| described(path, &read));
        require(seen, "actions_local_action_unreviewed")?;
        Ok(found)
    }
}

/// What one review is of.
#[derive(Clone, Copy, Debug)]
pub struct Review<'a> {
    /// The copy of the sources whose workflows are read.
    pub snapshot: &'a Path,
    /// The commit the change is compared with; the head when there is none.
    pub base: Option<&'a str>,
    /// Whether workflows below `vendor/` are read too.
    pub include_vendor: bool,
}

/// Review every pin the snapshot holds and the tree of its base does not,
/// and write the receipt at `output` when each one passed. The base is
/// the head or the full name of a commit that the head descends from.
fn reviewed(asking: &Asking<'_>, review: &Review<'_>, output: &Path) -> Outcome<()> {
    let base = review.base.unwrap_or("HEAD");
    require(
        base == "HEAD" || crate::commit(base),
        "actions_invalid_base",
    )?;
    asking.git(&["merge-base", "--is-ancestor", base, "HEAD"])?;
    let now = asking.inventory(Some(review.snapshot), base, review.include_vendor)?;
    let before = asking.inventory(None, base, review.include_vendor)?;
    let added: Vec<&String> = now.difference(&before).collect();
    require(added.len() <= MAX_CHANGED, "actions_changed_pin_limit")?;
    let allowed = allowed_licenses(review.snapshot)?;
    let allowed = Allowed::new(allowed.iter().map(String::as_str));
    let mut records = Vec::new();
    for pin in added {
        records.push(asking.review(pin, &allowed)?);
    }
    files::write_private(output, receipt(&records).as_bytes(), 0o600)?;
    Ok(())
}

/// Review the actions the snapshot pins and its base does not. A base that
/// is no ancestor of the head leaves no change to review: the review is
/// skipped and the summary says so. The review is kept as a command's
/// evidence is kept, under the name the reference ran it by.
pub fn check(context: &mut Context, programs: &dyn Programs, review: &Review<'_>) -> Outcome<()> {
    if let Some(base) = review.base {
        let git = programs.prerequisite("git")?;
        let related = [
            git.as_os_str(),
            OsStr::new("merge-base"),
            OsStr::new("--is-ancestor"),
            OsStr::new(base),
            OsStr::new("HEAD"),
        ];
        let how = Run {
            accepted: &[0, 1, 128],
            ..Run::default()
        };
        let (status, _) = context.run("actions-base-ancestor", &related, &how)?;
        if status != 0 {
            context.annotate([("rangeScan", "skipped_base_not_an_ancestor".into())]);
            return Ok(());
        }
    }
    let output = context.output().join(RECEIPT);
    let work = |context: &Context| {
        let asking = Asking {
            context,
            programs,
            ends: Instant::now() + REVIEW_TIME,
        };
        reviewed(&asking, review, &output)
    };
    let complaint = |failure: &Failure| format!("Action dependency review failed: {failure}\n");
    context.step("actions-dependency-review", work, complaint)
}

#[cfg(test)]
mod tests;
