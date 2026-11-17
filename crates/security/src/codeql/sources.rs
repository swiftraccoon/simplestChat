//! The exact bytes behind a finding's location: a source of the checkout
//! only when its whole working copy is what the selected revision
//! committed, and a native source of the build only as the member of the
//! pinned archive, remote patch or reviewed local overlay that its wrap
//! file declares. Nothing is downloaded, extracted or run: the archives
//! come from the private cache that the vendor verification fills, and a
//! missing pin, a missing cache entry or an extracted source that differs
//! all refuse the finding.

use super::{Sourced, Stop};
use crate::{
    Fault, commit,
    context::{self, Failure},
    digest,
    document::Document,
    files, paths, require,
    vendor::{
        Bounds, Problem,
        archive::{Budget, Kind, tar_each, unpacked},
        compare::{self, Files},
        manifest::{self, Change, Manifest, Source, Tree, Wrap, relative_path},
        tree, verify, wrap,
    },
};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read as _,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

/// The vendor record, below the checkout.
pub const MANIFEST: &str = "vendor/integrity.json";
/// Where the native build's wrap files are.
pub const SUBPROJECTS: &str = "vendor/mediasoup-sys-0.19.0/subprojects";
/// The most bytes of one source that is read.
pub const MAX_SOURCE: u64 = 4 * 1024 * 1024;
/// How long one committed file may take to be shown.
const SHOWN: Duration = Duration::from_secs(20);
/// The most bytes `git show` may complain in.
const COMPLAINT: u64 = 65536;
/// What the vendor's unpacking is told to say of a stream past its bound.
const EXPANDED: &str = "codeql_archive_expansion";

/// What `git show` prints of a committed object.
pub trait Shown {
    /// The exit status of `git show object` in the checkout `root`, and
    /// what it printed, of at most `limit` bytes.
    fn show(&self, root: &Path, object: &str, limit: u64) -> Sourced<(i32, Vec<u8>)>;
}

/// The `git` on the caller's search path, in the caller's environment.
#[derive(Clone, Copy, Debug, Default)]
pub struct Git;

impl Shown for Git {
    fn show(&self, root: &Path, object: &str, limit: u64) -> Sourced<(i32, Vec<u8>)> {
        let mut command = Command::new("git");
        command.args(["show", object]).current_dir(root);
        let asked = context::asked(&mut command, &[], SHOWN, (limit, COMPLAINT));
        asked.map_err(|failure| {
            Stop::Code(match failure {
                Failure::Fixed(code) => code,
                Failure::Command(_) | Failure::Prerequisite(_) => "codeql_git_failed",
            })
        })
    }
}

/// The whole of the regular source `name` of the checkout `root`, of at
/// most `limit` bytes, read through no link, once `git` shows exactly those
/// bytes for it at `revision`.
pub fn tracked_bytes(
    root: &Path,
    revision: &str,
    name: &str,
    limit: u64,
    git: &dyn Shown,
) -> Sourced<Vec<u8>> {
    relative_path(name)?;
    require(commit(revision), "codeql_revision")?;
    let path = root.join(name);
    require(paths::unaliased(&path), "codeql_source_symlink")?;
    let data = files::bounded(&path, limit, false)?;
    let (status, expected) = git.show(root, &format!("{revision}:{name}"), limit)?;
    require(
        status == 0 && expected == data,
        "codeql_source_revision_mismatch",
    )?;
    Ok(data)
}

/// Whether some directory above `key` is among `links`.
fn below_link(key: &str, links: &BTreeSet<String>) -> bool {
    key.match_indices('/')
        .any(|(at, _)| links.contains(&key[..at]))
}

/// The regular members of the archive `data` that `source` pins, by their
/// names. A symbolic link of a gzip tar archive is passed over as never
/// readable, and so is nothing below it: a member there refuses the
/// archive. Any other kind of member but a directory refuses it too.
pub fn archive_files(source: &Source, data: &[u8], bounds: &Bounds) -> Sourced<Files> {
    let authentic = data.len() <= bounds.source && digest(data) == source.sha256;
    require(authentic, "codeql_archive_digest")?;
    if source.format != "tar.gz" {
        return Ok(crate::vendor::archive::files(source, data, bounds)?);
    }
    let expanded = unpacked(data, bounds.content, EXPANDED).map_err(|problem| {
        if problem.0 == EXPANDED {
            Stop::Code(EXPANDED)
        } else {
            Stop::Problem(problem)
        }
    })?;
    let mut budget = Budget::new(bounds);
    let mut links = BTreeSet::new();
    let mut stopped = None;
    let walked = tar_each(&expanded, &mut |name, kind, size, content| {
        let judged = (|| -> Sourced<()> {
            require(kind != Kind::Other, "codeql_archive_kind")?;
            let key = budget.reserve(name, size, kind == Kind::Directory)?;
            require(!below_link(&key, &links), "codeql_archive_link_ancestor")?;
            match kind {
                Kind::Link => {
                    require(size == 0, "codeql_archive_link_size")?;
                    links.insert(key);
                }
                Kind::Directory => require(size == 0, "codeql_archive_directory_size")?,
                _ => {
                    let mut body = Vec::new();
                    let read = content.take(bounds.member + 1).read_to_end(&mut body);
                    read.map_err(|_| Problem("Unreadable tar archive".to_owned()))?;
                    require(body.len() as u64 == size, "codeql_archive_member_size")?;
                    budget.files.insert(key, body);
                }
            }
            Ok(())
        })();
        // The walk stops at the first refusal, which is answered as it is.
        judged.map_err(|stop| {
            stopped = Some(stop);
            Problem(String::new())
        })
    });
    if let Some(stop) = stopped {
        return Err(stop);
    }
    walked?;
    Ok(budget.files)
}

/// The directory of a wrap file's path, as the reference's paths name the
/// parent of one: `.` for a file at the top.
fn parent(path: &str) -> &str {
    path.rsplit_once('/').map_or(".", |(parent, _)| parent)
}

/// One wrap of the record, with the bytes of its file.
struct Pinned {
    wrap: Wrap,
    data: Vec<u8>,
}

/// The native sources that the record of the checkout declares, read only
/// from their declared roots, with the archives of the private offline
/// cache read within bounds.
pub struct NativeSources<'a> {
    root: PathBuf,
    revision: String,
    cache: PathBuf,
    git: &'a dyn Shown,
    bounds: Bounds,
    manifest_sha: String,
    manifest: Manifest,
    wraps: BTreeMap<String, Pinned>,
    archives: BTreeMap<String, Files>,
    contents: BTreeMap<String, (Files, Document)>,
    total_bytes: u64,
}

impl<'a> NativeSources<'a> {
    /// The record committed at `revision` in the checkout `root`, its wrap
    /// files, and the private cache `cache`, all bound before any source is
    /// resolved. `git` shows what was committed, and `bounds` are the vendor
    /// verification's.
    pub fn new(
        root: &Path,
        revision: &str,
        cache: &Path,
        git: &'a dyn Shown,
        bounds: Bounds,
    ) -> Sourced<Self> {
        let cache = verify::private_directory(cache, false)?;
        let record = bounds.record as u64;
        let manifest_bytes = tracked_bytes(root, revision, MANIFEST, record, git)?;
        let manifest = manifest::parse(&manifest_bytes)?;
        let mut sources = Self {
            root: root.to_path_buf(),
            revision: revision.to_owned(),
            cache,
            git,
            bounds,
            manifest_sha: digest(&manifest_bytes),
            manifest,
            wraps: BTreeMap::new(),
            archives: BTreeMap::new(),
            contents: BTreeMap::new(),
            total_bytes: 0,
        };
        for wrap in sources.manifest.wraps.clone() {
            require(parent(&wrap.path) == SUBPROJECTS, "codeql_wrap_root")?;
            let data = tracked_bytes(root, revision, &wrap.path, MAX_SOURCE, git)?;
            let text = std::str::from_utf8(&data).map_err(|_| Fault("codeql_wrap_unread"))?;
            let found = wrap::value(text, "wrap-file", "directory");
            let directory = found.ok_or(Fault("codeql_wrap_unread"))?;
            let directory = directory.ok_or(Fault("codeql_wrap_directory_missing"))?;
            let directory = relative_path(&directory)?;
            let plain = !directory.contains('/')
                && directory != "packagefiles"
                && directory != "packagecache";
            require(plain, "codeql_wrap_directory")?;
            let selected = format!("{SUBPROJECTS}/{directory}");
            require(
                !sources.wraps.contains_key(&selected),
                "codeql_duplicate_wrap_directory",
            )?;
            sources.wraps.insert(selected, Pinned { wrap, data });
        }
        Ok(sources)
    }

    /// The source `identifier` of the record.
    fn source(&self, identifier: &str) -> Sourced<Source> {
        let found = self.manifest.source(identifier).cloned();
        found.ok_or(Stop::Code("codeql_source_unknown"))
    }

    /// The members of the archive `source`, authenticated whole, with the
    /// bytes kept of every archive together held to the bound.
    pub fn archive(&mut self, source: &Source) -> Sourced<Files> {
        if let Some(files) = self.archives.get(&source.identifier) {
            return Ok(files.clone());
        }
        let data = verify::source_bytes(source, &self.cache, None, self.bounds.source)?;
        let files = archive_files(source, &data, &self.bounds)?;
        let kept: u64 = files.values().map(|body| body.len() as u64).sum();
        self.total_bytes += kept;
        require(
            self.total_bytes <= self.bounds.content,
            "codeql_native_content_limit",
        )?;
        self.archives
            .insert(source.identifier.clone(), files.clone());
        Ok(files)
    }

    /// The local overlay of `wrap`, every file of it as Git committed it
    /// and differing from its upstream archive only as the record says.
    pub fn overlay(&mut self, wrap: &Wrap) -> Sourced<Files> {
        let Some(patch) = &wrap.patch_directory else {
            return Ok(Files::new());
        };
        let prefix = format!("{SUBPROJECTS}/packagefiles/{patch}");
        let selected =
            self.manifest.trees.iter().filter(|tree| {
                prefix == tree.path || prefix.starts_with(&format!("{}/", tree.path))
            });
        let tree = selected.fold(None::<&Tree>, |longest, tree| match longest {
            Some(held) if held.path.len() >= tree.path.len() => Some(held),
            _ => Some(tree),
        });
        let tree = tree.ok_or(Stop::Code("codeql_overlay_manifest"))?.clone();
        let rest = &prefix[tree.path.len()..];
        let relative = rest.strip_prefix('/').unwrap_or(rest).to_owned();
        let upstream = if relative.is_empty() {
            tree.prefix.clone()
        } else {
            format!("{}/{relative}", tree.prefix)
        };
        let within = format!("{relative}/");
        let changes = tree
            .changes
            .iter()
            .filter(|item| relative.is_empty() || item.path.starts_with(&within))
            .map(|item| Change {
                path: if relative.is_empty() {
                    item.path.clone()
                } else {
                    item.path[within.len()..].to_owned()
                },
                upstream: item.upstream.clone(),
                vendored: item.vendored.clone(),
            })
            .collect();
        let files = tree::files(&self.root.join(&prefix), &self.bounds)?;
        for (name, data) in &files {
            let path = format!("{prefix}/{name}");
            let committed = tracked_bytes(&self.root, &self.revision, &path, MAX_SOURCE, self.git)?;
            require(*data == committed, "codeql_overlay_revision")?;
        }
        let original = self.archive(&self.source(&tree.source)?)?;
        let beneath = format!("{upstream}/");
        let original: Files = original
            .into_iter()
            .filter(|(name, _)| name.starts_with(&beneath))
            .collect();
        let declared = Tree {
            path: prefix,
            source: tree.source.clone(),
            prefix: upstream,
            changes,
        };
        let compared = compare::tree(&declared, &original, &files, &self.bounds)?;
        require(compared.errors.is_empty(), "codeql_overlay_deviation")?;
        Ok(files)
    }

    /// The files of the native source below `prefix`: its pinned archive,
    /// with the declared remote patch or the verified local overlay
    /// applied, and the evidence that binds them.
    pub fn content(&mut self, prefix: &str) -> Sourced<(Files, Document)> {
        if let Some(found) = self.contents.get(prefix) {
            return Ok(found.clone());
        }
        let pinned = self.wraps.get(prefix);
        let pinned = pinned.ok_or(Stop::Code("codeql_native_prefix"))?;
        let (wrap, wrap_data) = (pinned.wrap.clone(), pinned.data.clone());
        let overlays = self.overlay(&wrap)?;
        let patch = wrap.patch_directory.as_deref().unwrap_or("None");
        let mut local = vec![wrap.path.clone()];
        let overlaid = overlays
            .keys()
            .map(|name| format!("{SUBPROJECTS}/packagefiles/{patch}/{name}"));
        local.extend(overlaid);
        let listed = local.iter().map(String::as_str);
        wrap::validate(&wrap, &self.manifest, &wrap_data, listed)?;
        let source = self.source(&wrap.source)?;
        let directory = format!("{}/", prefix.rsplit('/').next().unwrap_or(prefix));
        let original = self.archive(&source)?;
        require(!original.is_empty(), "codeql_empty_archive")?;
        let rooted = original.keys().all(|name| name.starts_with(&directory));
        require(rooted, "codeql_archive_root")?;
        let strip = |files: Files| -> Files {
            let renamed = files
                .into_iter()
                .map(|(name, data)| (name[directory.len()..].to_owned(), data));
            renamed.collect()
        };
        let mut files = strip(original);
        let mut evidence = Document::record([
            ("sourceArchiveSha256", source.sha256.as_str().into()),
            ("sourceWrapSha256", digest(&wrap_data).into()),
            ("sourceManifestSha256", self.manifest_sha.as_str().into()),
        ]);
        if let Some(patch_source) = &wrap.patch_source {
            let patch_source = self.source(patch_source)?;
            let patches = self.archive(&patch_source)?;
            let rooted = patches.keys().all(|name| name.starts_with(&directory));
            require(!patches.is_empty() && rooted, "codeql_patch_root")?;
            files.extend(strip(patches));
            evidence.put("sourcePatchSha256", patch_source.sha256.as_str().into());
        }
        if !overlays.is_empty() {
            let identities: Vec<(String, Document)> = overlays
                .iter()
                .map(|(name, data)| (name.clone(), digest(data).into()))
                .collect();
            let bound = Document::Record(identities).compact();
            evidence.put("sourceOverlaySha256", digest(bound.as_bytes()).into());
            files.extend(overlays);
        }
        self.contents
            .insert(prefix.to_owned(), (files.clone(), evidence.clone()));
        Ok((files, evidence))
    }

    /// The evidence that binds the native source `name` of the checkout to
    /// its archive member, or nothing for a path below no declared root. An
    /// extracted copy of it that is there must hold exactly those bytes.
    pub fn identity(&mut self, name: &str) -> Sourced<Option<Document>> {
        relative_path(name)?;
        let prefixes: Vec<String> = self
            .wraps
            .keys()
            .filter(|prefix| name.starts_with(&format!("{prefix}/")))
            .cloned()
            .collect();
        require(prefixes.len() <= 1, "codeql_native_ambiguous")?;
        let Some(prefix) = prefixes.first() else {
            return Ok(None);
        };
        let member = &name[prefix.len() + 1..];
        let (files, evidence) = self.content(prefix)?;
        let expected = files
            .get(member)
            .filter(|body| body.len() as u64 <= MAX_SOURCE);
        let expected = expected.ok_or(Stop::Code("codeql_native_member"))?;
        let path = self.root.join(name);
        require(paths::unaliased(&path), "codeql_source_symlink")?;
        if path.exists() || path.is_symlink() {
            let extracted = files::bounded(&path, MAX_SOURCE, false)?;
            require(extracted == *expected, "codeql_extracted_source_mismatch")?;
        }
        let mut identity = evidence;
        identity.put("sourceMember", member.into());
        identity.put("sourceSha256", digest(expected).into());
        Ok(Some(identity))
    }
}

#[cfg(test)]
#[path = "sources_tests.rs"]
mod tests;
