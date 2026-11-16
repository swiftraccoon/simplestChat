//! What the secret scanner found in the image, held to what a review
//! names. A finding is bound to the region that its coordinates name in
//! the projection of one layer file, and only a region named once, by its
//! size and digest, can be reviewed: a review names exactly those bytes at
//! that path, so that changed bytes around them are new evidence.

use super::{small_hex, spans};
use crate::{
    Fault, Result, digest,
    document::Document,
    policy::{Exception, permitted},
    require, secrets,
};

/// The most bytes of a path in a review.
const MAX_PATH: usize = 4096;
/// The most bytes of what a fingerprint is taken of.
const MAX_IDENTITY: usize = 32768;
/// What every fingerprint is a digest of first.
const DOMAIN: &[u8] = b"simplestchat-image-secret-match\0";
/// A finding, a span or a map of projections that lacks a field or has
/// one of another kind.
const SHAPE: Fault = Fault("image_secret_shape");
/// What the report of the spans says.
const SCOPE: &str = "every regular layer file, including deleted files, and complete image config";
const COORDINATES: [(&str, &str); 4] = [
    ("StartLine", "startLine"),
    ("EndLine", "endLine"),
    ("StartColumn", "startColumn"),
    ("EndColumn", "endColumn"),
];

/// The fingerprint of the region that the resolved `span` names: a digest
/// of its rule, its path, its format, its size and its digest, never of
/// what it holds.
fn fingerprint(span: &Document, limits: &spans::Limits) -> Result<String> {
    let resolved = span.get("status") == Some(&"resolved".into());
    require(resolved, "image_secret_span_unresolved")?;
    // The binding has seen that the rule and the path are text.
    let rule = span.get("rule").and_then(Document::text).ok_or(SHAPE)?;
    let path = span.get("path").and_then(Document::text).ok_or(SHAPE)?;
    let size = span.get("spanBytes").ok_or(SHAPE)?;
    let checksum = span
        .get("spanSha256")
        .and_then(Document::text)
        .ok_or(SHAPE)?;
    let sized = size
        .whole()
        .is_some_and(|size| (1..=i128::from(limits.file)).contains(&size));
    require(sized, "image_secret_span_size")?;
    require(small_hex(checksum, 64), "image_secret_span_digest")?;
    let identity = Document::record([
        ("format", spans::FORMAT.into()),
        ("path", path.into()),
        ("projectionFormat", secrets::FORMAT.into()),
        ("rule", rule.into()),
        ("spanBytes", size.clone()),
        ("spanSha256", checksum.into()),
    ]);
    let encoded = super::license_identity::spelled(&identity).ok_or(SHAPE)?;
    require(encoded.len() <= MAX_IDENTITY, "image_secret_identity_size")?;
    let mut bytes = DOMAIN.to_vec();
    bytes.extend_from_slice(encoded.as_bytes());
    Ok(format!("image-match:{}", digest(&bytes)))
}

/// Whether `path` is a plain relative path of the image: its steps are
/// neither empty nor a dot, unless the path is the dot itself, none
/// leaves it, and it holds no control character. An empty path and one
/// that begins at the root have an empty step.
fn plain(path: &str) -> bool {
    let sized = path.len() <= MAX_PATH;
    let canonical = path == "." || path.split('/').all(|step| !matches!(step, "" | "."));
    let inside = !path.split('/').any(|step| step == "..");
    let controlled = path
        .chars()
        .any(|letter| letter < ' ' || letter == '\u{7f}');
    sized && canonical && inside && !controlled
}

/// Whether `rule` names a rule of the scanner: a letter or a digit, then
/// at most 127 of those, points, lines and dashes.
fn named(rule: &str) -> bool {
    let mut letters = rule.chars();
    let first = letters
        .next()
        .is_some_and(|letter| letter.is_ascii_alphanumeric());
    let rest = letters.clone().count() <= 127
        && letters.all(|letter| letter.is_ascii_alphanumeric() || "._-".contains(letter));
    first && rest
}

/// The record of one finding of the scanner, bound to its `span`, and the
/// path of the image file it was found in: the projection must be one of
/// `paths`, every coordinate a whole number within the bound, and the span
/// must say of it exactly what the finding and the map of projections say,
/// and nothing more.
fn binding<'a>(
    finding: &Document,
    paths: &'a Document,
    span: &Document,
    limits: &spans::Limits,
) -> Result<(Vec<(String, Document)>, &'a str)> {
    let file = finding.get("File").and_then(Document::text).ok_or(SHAPE)?;
    let entry = file.strip_prefix("/layers/").unwrap_or(file);
    let numbered = entry.strip_prefix("content-").is_some_and(|number| {
        number.len() == 6 && number.bytes().all(|digit| digit.is_ascii_digit())
    });
    let identity = paths.get(entry).filter(|_| numbered);
    let identity = identity.ok_or(Fault("image_secret_file_unknown"))?;
    let path = identity.get("path").and_then(Document::text).ok_or(SHAPE)?;
    let rule = finding
        .get("RuleID")
        .and_then(Document::text)
        .ok_or(SHAPE)?;
    require(plain(path), "image_secret_path")?;
    require(named(rule), "image_secret_rule")?;
    let format = identity.get("projectionFormat").ok_or(SHAPE)?;
    require(
        format.text() == Some(secrets::FORMAT),
        "image_secret_projection",
    )?;
    let mut expected = vec![
        ("rule".to_owned(), Document::from(rule)),
        ("path".to_owned(), path.into()),
    ];
    // Both digests are there before either is read, as the reference read
    // them.
    let file = identity.get("sha256").ok_or(SHAPE)?;
    let projection = identity.get("projectionSha256").ok_or(SHAPE)?;
    for (name, value) in [("fileSha256", file), ("projectionSha256", projection)] {
        let value = value.text().ok_or(SHAPE)?;
        require(small_hex(value, 64), "image_secret_file_digest")?;
        expected.push((name.to_owned(), value.into()));
    }
    for (field, name) in COORDINATES {
        let value = finding.get(field).ok_or(SHAPE)?;
        let within = value
            .whole()
            .is_some_and(|value| (0..=i128::from(limits.file)).contains(&value));
        require(within, "image_secret_coordinates")?;
        expected.push((name.to_owned(), value.clone()));
    }
    let bound = expected
        .iter()
        .all(|(name, value)| span.get(name) == Some(value));
    require(bound, "image_secret_span_binding")?;
    let status = span.get("status").and_then(Document::text);
    let status = status.filter(|status| matches!(*status, "resolved" | "ambiguous" | "unresolved"));
    let status = status.ok_or(Fault("image_secret_span_status"))?;
    let mut fields: Vec<&str> = expected.iter().map(|(name, _)| name.as_str()).collect();
    fields.push("status");
    if status == "resolved" {
        fields.extend(["spanBytes", "spanSha256"]);
    }
    let held = span.fields().unwrap_or_default();
    let same =
        held.len() == fields.len() && held.iter().all(|(name, _)| fields.contains(&name.as_str()));
    require(same, "image_secret_span_fields")?;
    let line = finding.get("StartLine").cloned().ok_or(SHAPE)?;
    expected.push(("line".to_owned(), line));
    expected.push(("status".to_owned(), status.into()));
    Ok((expected, path))
}

/// What the secret scanner's `value` says of the image, its findings bound
/// one by one to the `spans` that were found for them in the projections
/// of `paths`: a finding is blocked unless its region is named once and a
/// review waives exactly those bytes at that path.
pub fn secret_verdict(
    value: &Document,
    exceptions: &[Exception],
    paths: &Document,
    spans: &Document,
    limits: &spans::Limits,
) -> Result<Document> {
    let findings = value.items().ok_or(SHAPE)?;
    let regions = spans
        .get("findings")
        .and_then(Document::items)
        .ok_or(SHAPE)?;
    let named: Vec<&str> = spans
        .fields()
        .unwrap_or_default()
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    // The findings are there; the format must be the other field.
    let reported = named.len() == 2
        && spans.get("format") == Some(&spans::FORMAT.into())
        && findings.len() == regions.len()
        && findings.len() <= limits.findings;
    require(reported, "image_secret_span_report")?;
    let (mut blocked, mut waived) = (Vec::new(), Vec::new());
    for (finding, span) in findings.iter().zip(regions) {
        require(span.fields().is_some(), SHAPE.0)?;
        let (mut record, path) = binding(finding, paths, span, limits)?;
        let mut approved = false;
        if span.get("status") == Some(&"resolved".into()) {
            let fingerprint = fingerprint(span, limits)?;
            approved = permitted(exceptions, "gitleaks", &fingerprint, path);
            record.push(("fingerprint".to_owned(), fingerprint.into()));
            record.push((
                "spanBytes".to_owned(),
                span.get("spanBytes").cloned().ok_or(SHAPE)?,
            ));
            record.push((
                "spanSha256".to_owned(),
                span.get("spanSha256").cloned().ok_or(SHAPE)?,
            ));
        }
        if approved {
            waived.push(Document::Record(record));
        } else {
            blocked.push(Document::Record(record));
        }
    }
    Ok(Document::record([
        ("passed", blocked.is_empty().into()),
        ("blocked", Document::List(blocked)),
        ("waived", Document::List(waived)),
        ("scope", SCOPE.into()),
    ]))
}

#[cfg(test)]
#[path = "secrets_tests.rs"]
pub(crate) mod tests;
