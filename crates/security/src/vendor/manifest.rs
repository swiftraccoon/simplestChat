//! The reviewed record of the vendored sources: where every upstream byte
//! comes from and which deviations from it are maintained. It is read
//! strictly: a field that is missing, unknown or of another kind refuses
//! the whole record, and so does a source that nothing refers to.

use super::{Problem, Verdict, listed, problem, repr};
use crate::document::{Document, Unread};

/// The most declarations of one kind.
const MAX_DECLARATIONS: usize = 64;
/// The most characters of a path.
const MAX_PATH: usize = 1024;

/// One exact upstream input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// Its name in the record.
    pub identifier: String,
    /// Where it is fetched from.
    pub url: String,
    /// The SHA-256 of its bytes.
    pub sha256: String,
    /// `tar.gz`, `zip` or `file`.
    pub format: String,
}

/// One file that differs from upstream: its digests there and here, where
/// none says that it is not there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Change {
    /// The file, from the tree.
    pub path: String,
    /// What upstream has.
    pub upstream: Option<String>,
    /// What is vendored.
    pub vendored: Option<String>,
}

/// A whole upstream tree and its reviewed deviations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tree {
    /// Where it is vendored.
    pub path: String,
    /// The archive it comes from.
    pub source: String,
    /// The directory of the archive that holds it.
    pub prefix: String,
    /// The reviewed deviations.
    pub changes: Vec<Change>,
}

/// A pinned native source of the build and its overlay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Wrap {
    /// The wrap file.
    pub path: String,
    /// The archive it pins.
    pub source: String,
    /// The archive of its remote patch.
    pub patch_source: Option<String>,
    /// The directory of its local overlay.
    pub patch_directory: Option<String>,
    /// Where else its source may be fetched from.
    pub fallback_urls: Vec<String>,
}

/// The whole record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Manifest {
    /// The sources, in the record's order.
    pub sources: Vec<Source>,
    /// The trees.
    pub trees: Vec<Tree>,
    /// The files copied whole, each with its source.
    pub files: Vec<(String, String)>,
    /// The files this repository writes itself.
    pub maintained_files: Vec<String>,
    /// The wraps.
    pub wraps: Vec<Wrap>,
}

impl Manifest {
    /// The source `identifier`.
    pub fn source(&self, identifier: &str) -> Option<&Source> {
        self.sources
            .iter()
            .find(|source| source.identifier == identifier)
    }
}

/// A canonical relative path of printable ASCII: no step is empty or a dot
/// or two, and there is neither a backslash nor a colon.
pub fn relative_path(value: &str) -> Verdict<&str> {
    let plain =
        |character: char| (' '..='~').contains(&character) && character != '\\' && character != ':';
    let stepped = value
        .split('/')
        .all(|step| !matches!(step, "" | "." | ".."));
    if value.chars().count() > MAX_PATH || !stepped || !value.chars().all(plain) {
        return Err(problem(format!(
            "Unsafe or noncanonical path: {}",
            repr(value)
        )));
    }
    Ok(value)
}

fn string(value: &Document) -> Verdict<&str> {
    value
        .text()
        .ok_or_else(|| problem("Expected a JSON string"))
}

fn array(value: &Document) -> Verdict<&[Document]> {
    value
        .items()
        .ok_or_else(|| problem("Expected a JSON array"))
}

/// A whole lowercase SHA-256.
pub(super) fn digest(value: &Document) -> Verdict<&str> {
    let text = string(value)?;
    let digit = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
    if text.len() != 64 || !text.bytes().all(digit) {
        return Err(problem("Invalid SHA-256"));
    }
    Ok(text)
}

/// An address over HTTPS on its usual port, of printable characters,
/// without a user, a password or a fragment.
pub fn https_url(value: &str) -> Verdict<&str> {
    let refused = || problem("Source URLs must be credential-free HTTPS URLs");
    let (scheme, rest) = value.split_once(':').ok_or_else(refused)?;
    let after = rest.strip_prefix("//").ok_or_else(refused)?;
    let location = &after[..after.find(['/', '?', '#']).unwrap_or(after.len())];
    let fragment = after
        .split_once('#')
        .is_some_and(|(_, fragment)| !fragment.is_empty());
    let (user, host) = match location.rsplit_once('@') {
        Some((user, host)) => (Some(user), host),
        None => (None, location),
    };
    let (name, port) = match host.split_once('[') {
        Some((_, bracketed)) => {
            let (name, port) = bracketed.split_once(']').ok_or_else(refused)?;
            let address: Result<std::net::Ipv6Addr, _> = name.parse();
            address.map_err(|_| refused())?;
            (name, port.split_once(':').map_or("", |(_, port)| port))
        }
        None => host.split_once(':').unwrap_or((host, "")),
    };
    let usual = port.is_empty()
        || (port.bytes().all(|byte| byte.is_ascii_digit())
            && port.trim_start_matches('0') == "443");
    let printable = value
        .chars()
        .all(|character| ('!'..='~').contains(&character));
    let held = scheme.eq_ignore_ascii_case("https")
        && !name.is_empty()
        && user.is_none()
        && !fragment
        && usual
        && printable;
    if held { Ok(value) } else { Err(refused()) }
}

/// A record with exactly the `expected` fields.
pub(super) fn fields<'a>(value: &'a Document, expected: &[&str]) -> Verdict<&'a Document> {
    let held = value
        .fields()
        .ok_or_else(|| problem("Expected a JSON object"))?;
    let mut odd: Vec<&str> = held.iter().map(|(name, _)| name.as_str()).collect();
    odd.retain(|name| !expected.contains(name));
    odd.extend(expected.iter().filter(|name| value.get(name).is_none()));
    if odd.is_empty() {
        return Ok(value);
    }
    odd.sort_unstable();
    Err(problem(format!(
        "Unexpected manifest fields: {}",
        listed(&odd)
    )))
}

fn field<'a>(record: &'a Document, name: &str) -> &'a Document {
    record.get(name).unwrap_or(&Document::Null)
}

fn text<'a>(record: &'a Document, name: &str) -> Verdict<&'a str> {
    string(field(record, name))
}

/// Refuse a value that is declared twice.
fn unique<'a>(mut values: impl Iterator<Item = &'a str>, label: &str) -> Verdict<()> {
    let mut seen: Vec<&str> = Vec::new();
    if values.any(|value| {
        seen.contains(&value) || {
            seen.push(value);
            false
        }
    }) {
        return Err(problem(format!("Duplicate {label}")));
    }
    Ok(())
}

fn source(value: &Document) -> Verdict<Source> {
    let record = fields(value, &["id", "url", "sha256", "format"])?;
    let identifier = text(record, "id")?;
    let plain = |byte: u8| byte.is_ascii_lowercase() || byte.is_ascii_digit();
    let named = identifier.bytes().next().is_some_and(plain)
        && identifier.len() <= 128
        && identifier
            .bytes()
            .all(|byte| plain(byte) || b"._-".contains(&byte));
    if !named {
        return Err(problem("Invalid source identifier"));
    }
    let source = Source {
        identifier: identifier.to_owned(),
        url: https_url(text(record, "url")?)?.to_owned(),
        sha256: digest(field(record, "sha256"))?.to_owned(),
        format: text(record, "format")?.to_owned(),
    };
    if !["tar.gz", "zip", "file"].contains(&source.format.as_str()) {
        return Err(problem("Unsupported source format"));
    }
    Ok(source)
}

fn change(value: &Document) -> Verdict<Change> {
    let record = fields(value, &["path", "upstream_sha256", "vendored_sha256"])?;
    let side = |name: &str| match field(record, name) {
        Document::Null => Ok(None),
        given => digest(given).map(|digest| Some(digest.to_owned())),
    };
    let (upstream, vendored) = (side("upstream_sha256")?, side("vendored_sha256")?);
    if upstream == vendored {
        return Err(problem(
            "Stale allowance: identical upstream and vendored identities",
        ));
    }
    Ok(Change {
        path: relative_path(text(record, "path")?)?.to_owned(),
        upstream,
        vendored,
    })
}

fn tree(value: &Document) -> Verdict<Tree> {
    let record = fields(value, &["path", "source", "prefix", "changes"])?;
    let changes: Vec<Change> = array(field(record, "changes"))?
        .iter()
        .map(change)
        .collect::<Verdict<_>>()?;
    unique(
        changes.iter().map(|change| change.path.as_str()),
        "change path",
    )?;
    Ok(Tree {
        path: relative_path(text(record, "path")?)?.to_owned(),
        source: text(record, "source")?.to_owned(),
        prefix: relative_path(text(record, "prefix")?)?.to_owned(),
        changes,
    })
}

fn wrap(value: &Document) -> Verdict<Wrap> {
    let names = [
        "path",
        "source",
        "patch_source",
        "patch_directory",
        "fallback_urls",
    ];
    let record = fields(value, &names)?;
    let path = relative_path(text(record, "path")?)?.to_owned();
    let source = text(record, "source")?.to_owned();
    let patch_source = match field(record, "patch_source") {
        Document::Null => None,
        given => Some(string(given)?.to_owned()),
    };
    let patch_directory = match field(record, "patch_directory") {
        Document::Null => None,
        given => Some(relative_path(string(given)?)?.to_owned()),
    };
    let fallback = |url: &Document| Ok(https_url(string(url)?)?.to_owned());
    let fallback_urls = array(field(record, "fallback_urls"))?
        .iter()
        .map(fallback)
        .collect::<Verdict<_>>()?;
    Ok(Wrap {
        path,
        source,
        patch_source,
        patch_directory,
        fallback_urls,
    })
}

/// Hold the record to itself: its size, every source referred to and no
/// other, everything below `vendor/`, an archive for every tree and every
/// wrap, a raw file for every copied file, and nothing written here but
/// notices and the native component metadata.
fn referenced(manifest: &Manifest) -> Verdict<()> {
    let counts = [
        manifest.sources.len(),
        manifest.trees.len(),
        manifest.files.len(),
        manifest.wraps.len(),
        manifest.maintained_files.len(),
    ];
    if counts.iter().any(|count| *count > MAX_DECLARATIONS) {
        return Err(problem("Manifest declaration budget exceeded"));
    }
    let native = || {
        let wraps = manifest.wraps.iter();
        wraps.flat_map(|wrap| {
            std::iter::once(wrap.source.as_str()).chain(wrap.patch_source.as_deref())
        })
    };
    let referred: Vec<&str> = manifest
        .trees
        .iter()
        .map(|tree| tree.source.as_str())
        .chain(manifest.files.iter().map(|(_, source)| source.as_str()))
        .chain(native())
        .collect();
    let known = |name: &&str| manifest.source(name).is_some();
    let used = |source: &Source| referred.contains(&source.identifier.as_str());
    if !referred.iter().all(known) || !manifest.sources.iter().all(used) {
        return Err(problem("Missing or stale source records"));
    }
    let paths = manifest.trees.iter().map(|tree| &tree.path);
    let paths = paths.chain(manifest.files.iter().map(|(path, _)| path));
    let mut paths = paths.chain(&manifest.maintained_files);
    if paths.any(|path| !path.starts_with("vendor/")) {
        return Err(problem("Vendor records must stay inside vendor/"));
    }
    let raw = |name: &str| {
        manifest
            .source(name)
            .is_some_and(|source| source.format == "file")
    };
    if manifest.trees.iter().any(|tree| raw(&tree.source)) {
        return Err(problem("A tree requires an archive source"));
    }
    if manifest.files.iter().any(|(_, source)| !raw(source)) {
        return Err(problem("A copied file requires a raw file source"));
    }
    if native().any(raw) {
        return Err(problem("Native wraps require archive sources"));
    }
    let written =
        |path: &String| path.ends_with("/README.md") || path == "vendor/native-components.json";
    if !manifest.maintained_files.iter().all(written) {
        return Err(problem(
            "Only declared README/native-component metadata may be repository-authored",
        ));
    }
    Ok(())
}

/// What bytes are no document, said as the reference said it where it can
/// be.
pub(super) fn unread(unread: Unread) -> Problem {
    match unread {
        Unread::Duplicate(name) => problem(format!("Duplicate JSON key: {name}")),
        Unread::Invalid => problem("Invalid JSON"),
    }
}

/// The record that `data` is.
pub fn parse(data: &[u8]) -> Verdict<Manifest> {
    let document = Document::read(data).map_err(unread)?;
    let names = ["sources", "trees", "files", "maintained_files", "wraps"];
    let record = fields(&document, &names)?;
    let sources: Vec<Source> = array(field(record, "sources"))?
        .iter()
        .map(source)
        .collect::<Verdict<_>>()?;
    unique(
        sources.iter().map(|source| source.identifier.as_str()),
        "source identifier",
    )?;
    let trees: Vec<Tree> = array(field(record, "trees"))?
        .iter()
        .map(tree)
        .collect::<Verdict<_>>()?;
    let wraps: Vec<Wrap> = array(field(record, "wraps"))?
        .iter()
        .map(wrap)
        .collect::<Verdict<_>>()?;
    let mut files: Vec<(String, String)> = Vec::new();
    for item in array(field(record, "files"))? {
        let copied = fields(item, &["path", "source"])?;
        let path = relative_path(text(copied, "path")?)?;
        if files.iter().any(|(held, _)| held == path) {
            return Err(problem("Duplicate copied file"));
        }
        files.push((path.to_owned(), text(copied, "source")?.to_owned()));
    }
    let maintained = |item: &Document| Ok(relative_path(string(item)?)?.to_owned());
    let maintained_files: Vec<String> = array(field(record, "maintained_files"))?
        .iter()
        .map(maintained)
        .collect::<Verdict<_>>()?;
    unique(trees.iter().map(|tree| tree.path.as_str()), "tree path")?;
    unique(wraps.iter().map(|wrap| wrap.path.as_str()), "wrap path")?;
    unique(
        maintained_files.iter().map(String::as_str),
        "maintained path",
    )?;
    let manifest = Manifest {
        sources,
        trees,
        files,
        maintained_files,
        wraps,
    };
    referenced(&manifest)?;
    Ok(manifest)
}
