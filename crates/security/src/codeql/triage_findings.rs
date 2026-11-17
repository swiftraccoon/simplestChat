//! One finding's complete identity, the same for a SARIF result and for an
//! alert of GitHub's: the rule, the analyzer, the primary range, the
//! message as GitHub renders it, and the exact bytes of the source it
//! points at, committed or native, with every Rust file it includes. The
//! verdict keeps every finding: a reviewed one is waived, an unreviewed
//! high or critical one blocks, and any other is advisory.

use super::{
    MAX_SOURCE, REGION, SEVERITIES, bounded_file, digest, field, positive, stopped, string,
};
use crate::{
    Result,
    codeql::{
        rust_sources::included_sources,
        sources::{NativeSources, Shown},
    },
    commit,
    document::Document,
    paths,
    policy::{Exception, permitted},
    require,
    scope::vendor_path,
};
use std::path::Path;

/// The SHA-256 of the whole tracked file `name` of the checkout `root`,
/// once its working copy is what `revision` committed; a path that is not
/// canonical, that escapes or that passes a link is refused.
pub fn source_hash(root: &Path, revision: &str, name: &str, git: &dyn Shown) -> Result<String> {
    let canonical = name.split('/').all(|step| !matches!(step, "" | "." | ".."))
        && !name.contains(['\\', '\n', '\r', '\0']);
    require(canonical, "codeql_source_path")?;
    require(commit(revision), "codeql_revision")?;
    let selected = root.join(name);
    require(paths::unaliased(&selected), "codeql_source_symlink")?;
    let data = bounded_file(&selected, MAX_SOURCE)?;
    let object = format!("{revision}:{name}");
    let (status, original) = git.show(root, &object, MAX_SOURCE).map_err(stopped)?;
    require(
        status == 0 && data == original,
        "codeql_source_revision_mismatch",
    )?;
    Ok(crate::digest(&data))
}

/// The label of a numbered related-location link, `[text](12)`, where
/// `text` begins at the start of `rest` after its bracket: the label and
/// the length of the whole link, when the link is one.
fn link(rest: &str) -> Option<(&str, usize)> {
    let label = rest[1..]
        .find(['[', ']', '\n'])
        .map_or(rest.len(), |at| at + 1);
    let after = &rest[label..];
    let digits = after.strip_prefix("](")?;
    let count = digits.bytes().take_while(u8::is_ascii_digit).count();
    let closed = label > 1 && count > 0 && digits[count..].starts_with(')');
    closed.then(|| (&rest[1..label], label + 2 + count + 1))
}

/// `message` as GitHub renders it: each numbered related-location link of
/// SARIF becomes its plain label, and nothing else is changed.
pub(crate) fn rendered(message: &str) -> String {
    let mut text = String::with_capacity(message.len());
    let mut rest = message;
    while let Some(at) = rest.find('[') {
        text.push_str(&rest[..at]);
        rest = &rest[at..];
        match link(rest) {
            Some((label, length)) => {
                text.push_str(label);
                rest = &rest[length..];
            }
            None => {
                text.push('[');
                rest = &rest[1..];
            }
        }
    }
    text.push_str(rest);
    text
}

/// The complete identity of a finding of the rule `rule` of the analyzer
/// `version` at the record `location`, with the message `message`: its
/// range, the hash of its rendered message, the exact bytes of its source
/// (a native one through `sources`, any other as committed) and of every
/// Rust file it includes, its scope and the fingerprint of all of them.
#[expect(
    clippy::too_many_arguments,
    reason = "the source, the query, the location and the evidence are each their own"
)]
pub fn finding(
    root: &Path,
    revision: &str,
    rule: &str,
    version: &str,
    location: &Document,
    message: &str,
    sources: Option<&mut NativeSources<'_>>,
    git: &dyn Shown,
) -> Result<Document> {
    let name = string(field(location, "path")?)?;
    // Each bound is read in its order, and the first that fails refuses.
    let [start_line, end_line, start_column, end_column] =
        REGION.map(|key| field(location, key).and_then(positive));
    let region = [start_line?, end_line?, start_column?, end_column?];
    let [start_line, end_line, start_column, end_column] = region;
    require(
        end_line >= start_line && (end_line != start_line || end_column >= start_column),
        "codeql_region_order",
    )?;
    let rendered = rendered(message);
    let native = match sources {
        Some(sources) => sources.identity(name).map_err(stopped)?,
        None => None,
    };
    let bound = REGION
        .iter()
        .zip(region)
        .map(|(key, value)| ((*key).to_owned(), Document::from(value)));
    let mut identity = Document::record([
        ("rule", rule.into()),
        ("toolVersion", version.into()),
        ("path", name.into()),
        ("region", Document::Record(bound.collect())),
        ("messageSha256", crate::digest(rendered.as_bytes()).into()),
    ]);
    let first_party = native.is_none();
    if let Some(native) = native {
        for (key, value) in native.fields().unwrap_or_default() {
            identity.put(key, value.clone());
        }
    } else {
        let hash = source_hash(root, revision, name, git)?;
        identity.put("sourceSha256", hash.into());
    }
    if first_party && name.ends_with(".rs") && !vendor_path(name) {
        let included = included_sources(root, revision, name, git).map_err(stopped)?;
        if !included.is_empty() {
            let hashes = included
                .into_iter()
                .map(|(path, hash)| (path, Document::from(hash)));
            identity.put("includedSources", Document::Record(hashes.collect()));
        }
    }
    let scope = format!("{name}:{start_line}:{end_line}:{start_column}:{end_column}");
    let fingerprint = format!("codeql:{}", digest(&identity));
    identity.put("scope", scope.into());
    identity.put("fingerprint", fingerprint.into());
    Ok(identity)
}

/// The findings `findings` held to the reviews `reviews`: each reviewed one
/// waived, each other high or critical one blocking, every other one
/// advisory, and the whole passed only when nothing blocks.
pub fn verdict(findings: &[Document], reviews: &[Exception]) -> Result<Document> {
    let (mut blocked, mut waived, mut advisory) = (Vec::new(), Vec::new(), Vec::new());
    for item in findings {
        let severity = string(field(item, "severity")?)?;
        require(SEVERITIES.contains(&severity), "codeql_unknown_severity")?;
        let fingerprint = string(field(item, "fingerprint")?)?;
        let scope = string(field(item, "scope")?)?;
        let selected = if permitted(reviews, "codeql", fingerprint, scope) {
            &mut waived
        } else if severity == "high" || severity == "critical" {
            &mut blocked
        } else {
            &mut advisory
        };
        selected.push(item.clone());
    }
    Ok(Document::record([
        ("passed", blocked.is_empty().into()),
        ("blocked", blocked.into()),
        ("waived", waived.into()),
        ("advisory", advisory.into()),
    ]))
}
