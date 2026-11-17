//! The whole verification: the vendored directory is read, its record is
//! held to its rules, every source is taken from the cache or fetched and
//! authenticated, every tree is compared with its archive, and the
//! evidence is written: every difference, and either the report or what
//! is wrong.

use super::{
    Bounds, Problem, Verdict, archive,
    compare::{self, Files},
    manifest::{self, Manifest, Source, https_url},
    problem, tree,
};
use crate::{
    digest,
    document::Document,
    files,
    tools::{Answer, Curl, Transport},
};
use std::{
    os::unix::fs::{DirBuilderExt, MetadataExt},
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

const USAGE: &str =
    "usage: simplestchat-security vendor verify --cache DIR --output DIR [--root DIR] [--offline]";
/// How long one source may take to arrive.
const DOWNLOAD: Duration = Duration::from_secs(120);
/// How long every source together may take to arrive.
pub const WHOLE: Duration = Duration::from_secs(900);

/// What is verified: the checkout, the cache of its sources, the new
/// directory of the evidence, and the bounds.
#[derive(Clone, Copy, Debug)]
pub struct Verification<'a> {
    /// The checkout whose vendored directory is verified.
    pub root: &'a Path,
    /// Where the sources are kept under their digests.
    pub cache: &'a Path,
    /// The new directory of the evidence.
    pub output: &'a Path,
    /// The bounds.
    pub bounds: Bounds,
}

/// Where sources that the cache lacks come from: a transport, and the time
/// on its clock by which the last of them must have arrived.
pub struct Network<'a> {
    /// What fetches.
    pub transport: &'a mut dyn Transport,
    /// When no more is fetched.
    pub deadline: Duration,
}

impl Network<'_> {
    /// The bytes at the address of `source`, not yet authenticated.
    fn fetched(&mut self, source: &Source, limit: usize) -> Verdict<Vec<u8>> {
        let failed = |reason: &str| {
            let identifier = &source.identifier;
            problem(format!("Source download failed: {identifier}: {reason}"))
        };
        let left = self.deadline.saturating_sub(self.transport.now());
        if left.is_zero() {
            return Err(failed("no time is left"));
        }
        let address = https_url(&source.url)?;
        let within = left.min(DOWNLOAD);
        match self.transport.fetch(address, limit as u64, within) {
            Answer::Body { bytes, address } if https_url(&address).is_ok() => Ok(bytes),
            Answer::Body { .. } => Err(failed("its last address is no plain HTTPS address")),
            Answer::Status(status) => Err(failed(&format!("HTTP status {status}"))),
            Answer::Oversized => Err(failed("more bytes than its budget")),
            Answer::Late => Err(failed("no answer in time")),
            Answer::Failed => Err(failed("no response")),
        }
    }
}

/// The real directory `path` of the caller's that nobody else may enter,
/// made first when `create` says so.
pub(crate) fn private_directory(path: &Path, create: bool) -> Verdict<PathBuf> {
    let shown = path.display();
    if create {
        let made = std::fs::DirBuilder::new().mode(0o700).create(path);
        made.map_err(|_| problem(format!("Directory cannot be made: {shown}")))?;
    }
    let own = rustix::process::getuid().as_raw();
    let private = path
        .symlink_metadata()
        .is_ok_and(|held| held.is_dir() && held.uid() == own && held.mode() & 0o077 == 0);
    let real = path.canonicalize().ok().filter(|_| private);
    real.ok_or_else(|| {
        problem(format!(
            "Directory must be owned and private (0700): {shown}"
        ))
    })
}

/// The bytes of `source`: the cache's, or fetched and kept there under
/// their digest. Whichever they are, they have the digest of the record.
pub(crate) fn source_bytes(
    source: &Source,
    cache: &Path,
    network: Option<&mut Network<'_>>,
    limit: usize,
) -> Verdict<Vec<u8>> {
    let identifier = &source.identifier;
    let differs = || problem(format!("Source SHA-256 differs: {identifier}"));
    let path = cache.join(&source.sha256);
    if path.symlink_metadata().is_err() {
        let Some(network) = network else {
            return Err(problem(format!("Offline source missing: {identifier}")));
        };
        let data = network.fetched(source, limit)?;
        if digest(&data) != source.sha256 {
            return Err(differs());
        }
        // A new file under another name, then a second name for it: what
        // the cache holds under a digest is never written in place.
        let fetched = cache.join(format!("download-{}", source.sha256));
        let _ = std::fs::remove_file(&fetched);
        let kept = files::write_private(&fetched, &data, 0o600).is_ok()
            && std::fs::hard_link(&fetched, &path).is_ok();
        let _ = std::fs::remove_file(&fetched);
        if !kept {
            return Err(problem(format!("Source cannot be kept: {identifier}")));
        }
    }
    let data = tree::read_regular(&path, limit as u64)?;
    if digest(&data) != source.sha256 {
        return Err(differs());
    }
    Ok(data)
}

/// Every source of the record by its name, authenticated, and what the
/// report says of each.
fn sources(
    manifest: &Manifest,
    cache: &Path,
    mut network: Option<&mut Network<'_>>,
    bounds: &Bounds,
) -> Verdict<(Files, Vec<Document>)> {
    let mut ordered: Vec<&Source> = manifest.sources.iter().collect();
    ordered.sort_by(|left, right| left.identifier.cmp(&right.identifier));
    let (mut held, mut records, mut size) = (Files::new(), Vec::new(), 0_u64);
    for source in ordered {
        let body = source_bytes(source, cache, network.as_deref_mut(), bounds.source)?;
        size += body.len() as u64;
        if size > bounds.content {
            return Err(problem("Combined source byte budget exceeded"));
        }
        records.push(Document::record([
            ("id", source.identifier.as_str().into()),
            ("url", source.url.as_str().into()),
            ("sha256", source.sha256.as_str().into()),
            ("size", body.len().into()),
            ("format", source.format.as_str().into()),
        ]));
        held.insert(source.identifier.clone(), body);
    }
    Ok((held, records))
}

fn write(output: &Path, name: &str, content: &[u8]) -> Verdict<()> {
    let written = files::write_private(&output.join(name), content, 0o600);
    written.map_err(|_| problem(format!("Evidence cannot be written: {name}")))
}

/// Verify the vendored directory of a checkout against its record, with
/// the sources kept in the cache, and write the evidence into its new
/// directory. Without a network, a source that the cache lacks is refused.
pub fn verify(asked: &Verification<'_>, network: Option<&mut Network<'_>>) -> Verdict<()> {
    let bounds = &asked.bounds;
    let absent = asked.cache.symlink_metadata().is_err();
    let cache = private_directory(asked.cache, absent)?;
    let output = private_directory(asked.output, true)?;
    let local: Files = tree::files(&asked.root.join("vendor"), bounds)?
        .into_iter()
        .map(|(path, body)| (format!("vendor/{path}"), body))
        .collect();
    let missing = |path: &str| problem(format!("Missing vendor file: {path}"));
    let record = "vendor/integrity.json";
    let manifest_data = local.get(record).ok_or_else(|| missing(record))?;
    if manifest_data.len() > bounds.record {
        return Err(problem("Manifest size limit exceeded"));
    }
    let manifest = manifest::parse(manifest_data)?;
    compare::coverage(&manifest, &local)?;
    let (held, source_records) = sources(&manifest, &cache, network, bounds)?;
    let source = |identifier: &str| held.get(identifier).ok_or_else(|| missing(identifier));
    let (mut errors, mut reports, mut diff) = (Vec::new(), Vec::new(), Vec::new());
    for tree in &manifest.trees {
        let pinned = manifest.source(&tree.source);
        let pinned = pinned.ok_or_else(|| missing(&tree.source))?;
        let upstream = archive::files(pinned, source(&tree.source)?, bounds)?;
        let below = format!("{}/", tree.path);
        let selected: Files = local
            .iter()
            .filter_map(|(path, body)| Some((path.strip_prefix(&below)?.to_owned(), body.clone())))
            .collect();
        let compared = compare::tree(tree, &upstream, &selected, bounds)?;
        reports.push(compared.report);
        errors.extend(compared.errors);
        diff.extend(compared.diff);
        if diff.len() > bounds.differences {
            return Err(problem("Vendor diff budget exceeded"));
        }
    }
    let mut copied: Vec<&(String, String)> = manifest.files.iter().collect();
    copied.sort();
    let mut copies = Vec::new();
    for (path, identifier) in copied {
        let body = local.get(path).ok_or_else(|| missing(path))?;
        if body != source(identifier)? {
            errors.push(format!("Copied upstream file differs: {path}"));
        }
        copies.push(Document::record([
            ("path", path.as_str().into()),
            ("source", identifier.as_str().into()),
            ("sha256", digest(body).into()),
        ]));
    }
    write(&output, "vendor.diff", &diff)?;
    if !errors.is_empty() {
        let listed = errors.iter().map(|error| error.as_str().into()).collect();
        let failure = Document::record([("errors", Document::List(listed))]);
        write(&output, "failure.json", failure.pretty().as_bytes())?;
        return Err(problem(errors.join("; ")));
    }
    let mut maintained = Vec::new();
    for path in &manifest.maintained_files {
        let body = local.get(path).ok_or_else(|| missing(path))?;
        let sha256 = digest(body).into();
        maintained.push(Document::record([
            ("path", path.as_str().into()),
            ("sha256", sha256),
        ]));
    }
    let report = Document::record([
        ("manifest_sha256", digest(manifest_data).into()),
        ("diff_sha256", digest(&diff).into()),
        ("sources", source_records.into()),
        ("trees", reports.into()),
        ("files", copies.into()),
        ("maintained_files", maintained.into()),
    ]);
    write(&output, "report.json", report.sorted().pretty().as_bytes())
}

/// What a command line asks for: the checkout, the cache, the new
/// directory of the evidence, and whether nothing may be fetched.
fn asked(root: &Path, arguments: &[String]) -> Option<(PathBuf, PathBuf, PathBuf, bool)> {
    let (mut named, mut cache, mut output, mut offline) = (None, None, None, false);
    let (command, mut rest) = arguments
        .split_first()
        .map(|(first, rest)| (first, rest.iter()))?;
    while let Some(option) = rest.next() {
        if option == "--offline" && !offline {
            offline = true;
            continue;
        }
        let value = rest.next();
        let value = value.filter(|value| !value.is_empty() && !value.starts_with('-'))?;
        match option.as_str() {
            "--root" if named.is_none() => named = Some(PathBuf::from(value)),
            "--cache" if cache.is_none() => cache = Some(PathBuf::from(value)),
            "--output" if output.is_none() => output = Some(PathBuf::from(value)),
            _ => return None,
        }
    }
    let root = named.unwrap_or_else(|| root.to_path_buf());
    (command == "verify").then_some((root, cache?, output?, offline))
}

/// Verify the vendored directory of `root` with the sources in `cache`,
/// fetching what the cache lacks within the time every source may take.
pub fn fetching(asked: &Verification<'_>) -> Verdict<()> {
    let mut transport = Curl::default();
    let deadline = transport.now() + WHOLE;
    let mut network = Network {
        transport: &mut transport,
        deadline,
    };
    verify(asked, Some(&mut network))
}

/// Run `vendor verify --cache DIR --output DIR [--root DIR] [--offline]`
/// for the checkout `root`: status 0 when the vendored directory is what
/// its record says, and otherwise one line on standard error and status 1.
pub fn run(root: &Path, arguments: &[String]) -> ExitCode {
    let Some((root, cache, output, offline)) = asked(root, arguments) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let root = root.canonicalize().unwrap_or(root);
    let asked = Verification {
        root: &root,
        cache: &cache,
        output: &output,
        bounds: Bounds::REVIEWED,
    };
    let verified = if offline {
        verify(&asked, None)
    } else {
        fetching(&asked)
    };
    match verified {
        Ok(()) => ExitCode::SUCCESS,
        Err(Problem(complaint)) => {
            eprintln!("Vendor integrity failed: {complaint}");
            ExitCode::from(1)
        }
    }
}

#[cfg(test)]
#[path = "verify_tests.rs"]
mod tests;
