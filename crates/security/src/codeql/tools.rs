//! The pinned analyzer and its bundled query suites, the same locally and
//! in CI: one shared pin names the CLI, every language's extractor and
//! query pack, and each platform's bundle by its size and SHA-256. A bundle
//! is downloaded only when the private cache lacks it, authenticated before
//! it is listed or extracted, and published there whole; mutable query
//! packs are never installed.

use super::{alike, lines, unmarked};
use crate::{
    context::{self, Context, Failure, Outcome, Run},
    document::Document,
    files, paths, require,
};
use std::{
    ffi::{OsStr, OsString},
    fs::{self, DirBuilder},
    io::{Read as _, Write},
    os::unix::{ffi::OsStrExt, fs::DirBuilderExt},
    path::{Path, PathBuf},
    time::Duration,
};

/// The shared pin, below the checkout.
pub const PIN: &str = "security/codeql-toolchain.json";
/// The workflow whose action revisions the pin names.
const WORKFLOW: &str = ".github/workflows/codeql.yml";
/// Every language the pin covers.
pub const LANGUAGES: [&str; 5] = [
    "actions",
    "javascript-typescript",
    "python",
    "rust",
    "c-cpp",
];
/// The first-party languages the local runner may analyze without a vendor
/// scan, and whose reports it may upload: every one but the vendored native
/// sources.
pub const AUTOMATED_LANGUAGES: [&str; 4] = ["actions", "javascript-typescript", "python", "rust"];
/// The languages the checkout has sources of: those whose security analysis
/// the CodeQL workflow stores for every revision, which the health and triage
/// gates require, and which a first-party local run covers when it names no
/// language. The checkout tracks no Python, and the CodeQL extractor refuses a
/// language with no source.
pub const STORED_LANGUAGES: [&str; 3] = ["actions", "javascript-typescript", "rust"];
/// The configuration of a first-party analysis, below the checkout.
pub const FIRST_PARTY_CONFIG: &str = "security/codeql/first-party.yml";
/// The most bytes of the pin.
const MAX_PIN: u64 = 16384;
/// The most bytes of a query pack's metadata.
const MAX_METADATA: u64 = 65536;
/// The most bytes of an installation's receipt.
const MAX_RECEIPT: u64 = 4096;
/// The most members a bundle may list.
pub const MAX_MEMBERS: usize = 250_000;
/// The two suites of every language.
const CATEGORIES: [&str; 2] = ["security", "quality-advisory"];
/// How long the download may take, and the listing and the extraction.
const DOWNLOAD: Duration = Duration::from_secs(610);
const LISTING: Duration = Duration::from_secs(180);
const EXTRACTION: Duration = Duration::from_secs(300);
/// A document the reference's reader refused.
const UNREAD: Failure = Failure::Fixed("codeql_tools_json");
/// A field the reference looked for and did not find, or of another kind.
const MISSING: Failure = Failure::Fixed("codeql_tools_field");

/// One field of generated metadata that must be given exactly once, with
/// the code that refuses it otherwise.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    /// The field's name.
    pub name: &'static str,
    /// The code of a field that is missing, repeated or different.
    pub code: &'static str,
}

impl Field {
    /// A query pack's name.
    pub const NAME: Self = Self::of("name", "codeql_metadata_name");
    /// A query pack's version.
    pub const VERSION: Self = Self::of("version", "codeql_metadata_version");
    /// The CLI version that wrote the metadata.
    pub const CLI_VERSION: Self = Self::of("cliVersion", "codeql_metadata_cliVersion");
    /// A database's language.
    pub const PRIMARY_LANGUAGE: Self =
        Self::of("primaryLanguage", "codeql_metadata_primaryLanguage");
    /// Whether a database is finished.
    pub const FINALISED: Self = Self::of("finalised", "codeql_metadata_finalised");
    /// Whether a database is the base of an overlay.
    pub const OVERLAY_BASE_DATABASE: Self =
        Self::of("overlayBaseDatabase", "codeql_metadata_overlayBaseDatabase");
    /// Whether a database is an overlay.
    pub const OVERLAY_DATABASE: Self =
        Self::of("overlayDatabase", "codeql_metadata_overlayDatabase");
    /// How a database was built.
    pub const BUILD_MODE: Self = Self::of("buildMode", "codeql_metadata_buildMode");

    const fn of(name: &'static str, code: &'static str) -> Self {
        Self { name, code }
    }
}

/// The field `name` of the record `value`.
fn part<'a>(value: &'a Document, name: &str) -> Outcome<&'a Document> {
    value.fields().ok_or(MISSING)?;
    value.get(name).ok_or(MISSING)
}

/// The record `value`.
fn record(value: &Document) -> Outcome<&Document> {
    value.fields().map(|_| value).ok_or(MISSING)
}

/// The text `value`.
fn text(value: &Document) -> Outcome<&str> {
    value.text().ok_or(MISSING)
}

/// The JSON document `data`, read as the reference read one: a field named
/// twice and a number that is not finite refuse it.
fn document(data: &[u8]) -> Outcome<Document> {
    Document::read(unmarked(data)).map_err(|_| UNREAD)
}

/// Every revision the workflow `text` pins one of the CodeQL actions at:
/// the forty small hexadecimal digits after `github/codeql-action/`, a
/// name of small letters and hyphens, and `@`.
fn action_revisions(text: &str) -> std::collections::BTreeSet<&str> {
    const ACTION: &str = "github/codeql-action/";
    let mut found = std::collections::BTreeSet::new();
    let mut rest = text;
    while let Some(at) = rest.find(ACTION) {
        let after = &rest[at + ACTION.len()..];
        let name = after
            .bytes()
            .take_while(|byte| byte.is_ascii_lowercase() || *byte == b'-')
            .count();
        let revision = after[name..]
            .strip_prefix('@')
            .and_then(|tail| tail.get(..40))
            .filter(|revision| crate::native::revision(revision));
        match revision {
            Some(revision) if name > 0 => {
                found.insert(revision);
                rest = &after[name + 41..];
            }
            _ => rest = &rest[at + 1..],
        }
    }
    found
}

/// The shared pin of the checkout `root`, once its schema, its languages
/// and the workflow's action revisions agree with it.
pub fn pin(root: &Path) -> Outcome<Document> {
    let result = document(&files::bounded(&root.join(PIN), MAX_PIN, false)?)?;
    let schema = part(&result, "schemaVersion")?;
    require(alike(schema, &1_u64.into()), "codeql_toolchain_schema")?;
    let languages = record(part(&result, "languages")?)?.names();
    require(
        languages == LANGUAGES.into_iter().collect(),
        "codeql_toolchain_languages",
    )?;
    let workflow = fs::read(root.join(WORKFLOW)).map_err(|error| files::unusable(&error))?;
    let workflow = String::from_utf8(workflow).map_err(|_| UNREAD)?;
    let revision = text(part(&result, "actionRevision")?)?;
    let pinned = action_revisions(&workflow) == [revision].into_iter().collect();
    require(pinned, "codeql_action_pin_mismatch")?;
    Ok(result)
}

/// The pin of `language`'s extractor and query pack.
pub fn language_pin(root: &Path, language: &str) -> Outcome<Document> {
    require(LANGUAGES.contains(&language), "codeql_language")?;
    let pinned = pin(root)?;
    Ok(record(part(part(&pinned, "languages")?, language)?)?.clone())
}

/// Require exactly one value of `field` in the generated metadata `text`,
/// and that it is `expected`, as the reference's one pattern finds them: a
/// line's start, white space, which may end lines, the name and a colon,
/// white space again, and the rest of a line, which must be all there is.
pub fn scalar(text: &str, field: Field, expected: &str) -> Outcome<()> {
    let mut found = Vec::new();
    let mut from = 0;
    let starts = std::iter::once(0).chain(text.match_indices('\n').map(|(at, _)| at + 1));
    for start in starts {
        // A start within the last value is no start of the pattern's own.
        if start < from {
            continue;
        }
        let rest = &text[start..];
        let named = rest.trim_start_matches(crate::policy::space);
        let Some(after) = named
            .strip_prefix(field.name)
            .and_then(|after| after.strip_prefix(':'))
        else {
            continue;
        };
        let at = text.len() - after.len();
        // The white space after the colon gives back what the value needs:
        // it ends where the value can begin on something but a line end.
        let spaced = after.len() - after.trim_start_matches(crate::policy::space).len();
        let ends = after[..spaced].char_indices().map(|(taken, _)| taken);
        let begin = ends
            .chain([spaced])
            .rev()
            .map(|taken| at + taken)
            .find(|begin| {
                text[*begin..]
                    .chars()
                    .next()
                    .is_some_and(|next| next != '\n')
            });
        let Some(begin) = begin else {
            continue;
        };
        let end = text[begin..]
            .find('\n')
            .map_or(text.len(), |line| begin + line);
        found.push(&text[begin..end]);
        from = end;
    }
    Ok(require(found == [expected], field.code)?)
}

/// The bundled suite of `category` for `language` beside the CLI `codeql`,
/// once the pack's metadata names exactly the pinned pack, its version and
/// the pinned CLI.
pub fn suite(root: &Path, codeql: &Path, language: &str, category: &str) -> Outcome<PathBuf> {
    let selected = language_pin(root, language)?;
    let extractor = text(part(&selected, "extractor")?)?;
    let version = text(part(&selected, "queriesVersion")?)?;
    let queries = format!("{extractor}-queries");
    let parent = codeql.parent().unwrap_or(codeql);
    let pack = parent.join("qlpacks/codeql").join(&queries).join(version);
    let metadata = files::bounded(&pack.join("qlpack.yml"), MAX_METADATA, false)?;
    let metadata = String::from_utf8(metadata).map_err(|_| UNREAD)?;
    scalar(&metadata, Field::NAME, &format!("codeql/{queries}"))?;
    scalar(&metadata, Field::VERSION, version)?;
    let pinned = pin(root)?;
    scalar(
        &metadata,
        Field::CLI_VERSION,
        text(part(&pinned, "cliVersion")?)?,
    )?;
    require(CATEGORIES.contains(&category), "codeql_suite")?;
    let queries = if category == "security" {
        "security-extended"
    } else {
        "security-and-quality"
    };
    let result = pack
        .join("codeql-suites")
        .join(format!("{extractor}-{queries}.qls"));
    require(result.is_file(), "codeql_suite_missing")?;
    Ok(result)
}

/// The private directory `path`, made when it is not there; one that is
/// there already must be a directory.
fn owned_directory(path: &Path) -> Outcome<()> {
    match DirBuilder::new().mode(0o700).create(path) {
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && path.is_dir() => Ok(()),
        made => made.map_err(|error| files::unusable(&error).into()),
    }
}

/// `path` below `output`, spelled as the reference spelled a path.
fn below(output: &Path, name: &str) -> OsString {
    paths::path(output.join(name).as_os_str()).into_os_string()
}

/// Keep the analyzer's configuration, caches and temporary files in the
/// invocation's own directory, for every later command.
pub fn configure_environment(context: &mut Context) -> Outcome<()> {
    let output = context.output().to_path_buf();
    for name in ["tmp", "config", "cache"] {
        owned_directory(&output.join(name))?;
    }
    let mut options = OsString::from("-Duser.home=");
    options.push(below(&output, "home"));
    options.push(" -Djava.io.tmpdir=");
    options.push(below(&output, "tmp"));
    context.set("JAVA_TOOL_OPTIONS", options);
    context.set("TMPDIR", below(&output, "tmp"));
    context.set("XDG_CONFIG_HOME", below(&output, "config"));
    context.set("XDG_CACHE_HOME", below(&output, "cache"));
    Ok(())
}

/// Refuse a CLI or a query suite other than the pinned ones, for every
/// language of `languages`, before any analysis.
pub fn verify(context: &mut Context, codeql: &Path, languages: &[&str]) -> Outcome<()> {
    configure_environment(context)?;
    require(
        codeql.is_absolute() && codeql.is_file(),
        "codeql_binary_required",
    )?;
    let program = paths::path(codeql.as_os_str());
    let argv = [
        program.as_os_str(),
        OsStr::new("version"),
        OsStr::new("--format=json"),
    ];
    let (_, printed) = context.run("codeql-version", &argv, &Run::default())?;
    let result = document(&printed)?;
    let version = part(&result, "version")?;
    let root = context.root().to_path_buf();
    let pinned = pin(&root)?;
    require(
        alike(version, part(&pinned, "cliVersion")?),
        "codeql_cli_version",
    )?;
    for language in languages {
        for category in CATEGORIES {
            suite(&root, codeql, language, category)?;
        }
    }
    Ok(())
}

/// The SHA-256 of the regular file `path`, which is no link and has
/// exactly `expected_size` bytes, read a mebibyte at a time.
pub fn archive_digest(path: &Path, expected_size: u64) -> Outcome<String> {
    use sha2::Digest as _;
    let regular = path.is_file() && !path.is_symlink();
    require(regular, "codeql_archive_file")?;
    let size = path
        .metadata()
        .map_err(|error| files::unusable(&error))?
        .len();
    require(size == expected_size, "codeql_archive_size")?;
    let mut file = fs::File::open(path).map_err(|error| files::unusable(&error))?;
    let mut hashed = sha2::Sha256::new();
    let mut chunk = vec![0; 1024 * 1024];
    loop {
        let read = file
            .read(&mut chunk)
            .map_err(|error| files::unusable(&error))?;
        if read == 0 {
            break;
        }
        hashed.update(&chunk[..read]);
    }
    Ok(crate::hexadecimal(&hashed.finalize()))
}

/// The name the pin gives the bundle of a host on `platform`, as the
/// reference's interpreter names its platform, with the machine
/// `machine`: a Mac has one bundle for every machine.
pub fn bundle_platform(platform: &str, machine: &str) -> String {
    if platform == "darwin" {
        platform.to_owned()
    } else {
        format!("{platform}-{machine}")
    }
}

/// The name the pin gives this host's bundle.
pub fn host_bundle() -> String {
    let platform = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    };
    let name = rustix::system::uname();
    bundle_platform(platform, &name.machine().to_string_lossy())
}

/// Whether the bundle's member `member` lies below its one top directory
/// `codeql`, with no parent step and from no root.
fn contained(member: &str) -> bool {
    let mut steps = member
        .split('/')
        .filter(|step| !step.is_empty() && *step != ".");
    !member.starts_with('/')
        && steps.next() == Some("codeql")
        && member.split('/').all(|step| step != "..")
}

/// How many names a staging directory is tried under, as many as the
/// reference's library tried.
const STAGING_NAMES: usize = 20;

/// Eight letters, digits or underscores read from `random`, as the
/// reference's library named a new directory.
fn random_name(random: &mut fs::File) -> Outcome<String> {
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789_";
    let mut bytes = [0_u8; 8];
    random
        .read_exact(&mut bytes)
        .map_err(|error| files::unusable(&error))?;
    let letter = |byte: &u8| char::from(LETTERS[usize::from(*byte) % LETTERS.len()]);
    Ok(bytes.iter().map(letter).collect())
}

/// A new private directory below `parent` whose name begins with
/// `.codeql-install-` and ends with what `names` gives: a name that is
/// taken is given up for the next one, twenty in all, and any other
/// failure is the failure at once.
pub(crate) fn staging(
    parent: &Path,
    names: &mut dyn FnMut() -> Outcome<String>,
) -> Outcome<PathBuf> {
    for _ in 0..STAGING_NAMES {
        let path = parent.join(format!(".codeql-install-{}", names()?));
        match DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(files::unusable(&error).into()),
        }
    }
    // The reference's library said that no name was left as a name that
    // is taken says it.
    Err(Failure::Fixed("tool_entry_unavailable"))
}

/// Where an installation's programs come from, and which bundle it takes.
#[derive(Clone, Copy, Debug)]
pub struct Host<'a> {
    /// The pin's name for this host's bundle.
    pub bundle: &'a str,
    /// The caller's search path, which `curl` and `tar` are found on.
    pub search: Option<&'a OsStr>,
}

/// The pinned bundle's CLI below the private cache of the checkout: the
/// one installed there, once its receipt is the pin's, or else the bundle
/// downloaded into the invocation's directory, authenticated, listed, and
/// extracted into a new directory that is published whole.
pub fn install(context: &mut Context, host: &Host<'_>) -> Outcome<PathBuf> {
    let root = context.root().to_path_buf();
    let pinned = pin(&root)?;
    let bundles = record(part(&pinned, "bundles")?)?;
    require(
        bundles.get(host.bundle).is_some(),
        "codeql_platform_unsupported",
    )?;
    let asset = record(part(bundles, host.bundle)?)?;
    let expected = text(part(asset, "sha256")?)?.to_owned();
    let size = part(asset, "bytes")?.whole();
    let size = size
        .and_then(|size| u64::try_from(size).ok())
        .filter(|size| *size > 0);
    let size = size.ok_or(Failure::Fixed("codeql_archive_bound"))?;
    let directory = root.join(".cache/codeql-tools").join(&expected);
    let receipt = || {
        Document::record([
            ("archiveSha256", expected.as_str().into()),
            ("archiveBytes", size.into()),
        ])
    };
    if directory.exists() {
        let held = files::bounded(&directory.join("receipt.json"), MAX_RECEIPT, false)?;
        let held = document(&held)?;
        require(
            alike(record(&held)?, &receipt()),
            "codeql_installation_receipt",
        )?;
        return Ok(paths::path(directory.join("codeql/codeql").as_os_str()));
    }
    let archive = PathBuf::from(below(context.output(), "codeql-bundle.tar.zst"));
    let curl = context::executable("curl", host.search)?;
    let address = text(part(asset, "url")?)?;
    let limit = size.to_string();
    let download: [&OsStr; 16] = [
        curl.as_os_str(),
        "--fail".as_ref(),
        "--show-error".as_ref(),
        "--location".as_ref(),
        "--proto".as_ref(),
        "=https".as_ref(),
        "--tlsv1.2".as_ref(),
        "--connect-timeout".as_ref(),
        "20".as_ref(),
        "--max-time".as_ref(),
        "600".as_ref(),
        "--max-filesize".as_ref(),
        limit.as_ref(),
        "--output".as_ref(),
        archive.as_os_str(),
        address.as_ref(),
    ];
    let within = |within| Run {
        within,
        ..Run::default()
    };
    context.run("codeql-download", &download, &within(DOWNLOAD))?;
    let digest = archive_digest(&archive, size)?;
    require(digest == expected, "codeql_archive_digest")?;
    // The archive is authenticated before it is listed or extracted.
    let tar = context::executable("tar", host.search)?;
    let listing = [tar.as_os_str(), "-tf".as_ref(), archive.as_os_str()];
    let (_, listed) = context.run("codeql-archive-list", &listing, &within(LISTING))?;
    let listed = String::from_utf8(listed).map_err(|_| Failure::Fixed("codeql_archive_listing"))?;
    let members = lines(&listed);
    require(
        !members.is_empty() && members.len() <= MAX_MEMBERS,
        "codeql_archive_members",
    )?;
    let contained = members.iter().all(|member| contained(member));
    require(contained, "codeql_archive_path")?;
    let parent = directory.parent().unwrap_or(&directory);
    if let Some(above) = parent.parent() {
        let made = DirBuilder::new().recursive(true).mode(0o777).create(above);
        made.map_err(|error| files::unusable(&error))?;
    }
    owned_directory(parent)?;
    let mut random = fs::File::open("/dev/urandom").map_err(|error| files::unusable(&error))?;
    let staged = staging(parent, &mut || random_name(&mut random))?;
    let staged = paths::path(staged.as_os_str());
    let published = publish(context, &tar, &archive, &staged, &directory, &receipt());
    if staged.exists() {
        let _ = fs::remove_dir_all(&staged);
    }
    published?;
    Ok(paths::path(directory.join("codeql/codeql").as_os_str()))
}

/// Extract `archive` with `tar` into `staged`, write its receipt there and
/// publish it as `directory`, which must not have appeared meanwhile.
fn publish(
    context: &mut Context,
    tar: &Path,
    archive: &Path,
    staged: &Path,
    directory: &Path,
    receipt: &Document,
) -> Outcome<()> {
    let extraction = [
        tar.as_os_str(),
        "-xf".as_ref(),
        archive.as_os_str(),
        "-C".as_ref(),
        staged.as_os_str(),
    ];
    let how = Run {
        within: EXTRACTION,
        ..Run::default()
    };
    context.run("codeql-extract", &extraction, &how)?;
    let written = format!("{}\n", receipt.spaced());
    files::write_private(&staged.join("receipt.json"), written.as_bytes(), 0o600)?;
    let absent = directory.symlink_metadata().is_err();
    require(absent, "codeql_cache_appeared")?;
    fs::rename(staged, directory).map_err(|error| files::unusable(&error).into())
}

/// What `codeql tools` asks for.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Asked {
    /// The CLI to verify instead of the pinned bundle's.
    pub codeql: Option<PathBuf>,
    /// The one language to verify instead of every one.
    pub language: Option<String>,
    /// The new private directory of the evidence.
    pub output: PathBuf,
}

/// Verify the CLI `asked` names, or the pinned bundle's once it is
/// installed, below the checkout `root`, with its evidence in a new
/// private directory, and answer the CLI's path.
pub fn tools(
    root: &Path,
    asked: &Asked,
    host: &Host<'_>,
    ambient: &dyn Fn(&str) -> Option<OsString>,
) -> Outcome<PathBuf> {
    require(asked.output.is_absolute(), "codeql_output_absolute")?;
    let made = DirBuilder::new().mode(0o700).create(&asked.output);
    made.map_err(|error| files::unusable(&error))?;
    let limits = context::Limits::REVIEWED;
    let mut context = Context::new(root, &asked.output, limits, ambient)?;
    let codeql = match &asked.codeql {
        Some(codeql) => codeql.clone(),
        None => install(&mut context, host)?,
    };
    let languages: Vec<&str> = match &asked.language {
        Some(language) => vec![language.as_str()],
        None => LANGUAGES.to_vec(),
    };
    verify(&mut context, &codeql, &languages)?;
    Ok(codeql)
}

/// The exit status of `codeql tools`: the verified CLI's path on `out`, or
/// nothing at all for a refusal.
pub fn command(
    root: &Path,
    asked: &Asked,
    host: &Host<'_>,
    ambient: &dyn Fn(&str) -> Option<OsString>,
    out: &mut dyn Write,
) -> u8 {
    let Ok(codeql) = tools(root, asked, host, ambient) else {
        return 1;
    };
    let mut line = codeql.into_os_string().as_bytes().to_vec();
    line.push(b'\n');
    u8::from(out.write_all(&line).is_err())
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
