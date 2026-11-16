//! The licences of the image's packages, held to the image's policy. A
//! package whose every licence record has an expression is allowed when
//! the policy allows the expression they make together; any other is a
//! finding, which only a review of its exact declaration and package
//! waives. The application itself is not published: it declares no
//! licence, and that is what it must declare.

use super::license_identity::raw_license_identity;
use crate::{
    Fault, Result, digest,
    document::Document,
    policy::{Exception, permitted},
    require,
    spdx::{Allowed, permits},
};

/// A package or a policy that lacks a field or has one of another kind.
const SHAPE: Fault = Fault("image_license_shape");

/// Whether the reference's interpreter would take `value` for true.
fn truthy(value: &Document) -> bool {
    match value {
        Document::Null => false,
        Document::Bool(flag) => *flag,
        Document::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Document::Text(text) => !text.is_empty(),
        Document::List(items) => !items.is_empty(),
        Document::Record(fields) => !fields.is_empty(),
    }
}

/// The scope that a review of `package` names: its address, or its name
/// and its version when it has none.
fn scope(package: &Document) -> Result<String> {
    match package.get("purl").filter(|purl| truthy(purl)) {
        Some(purl) => purl.text().map(str::to_owned).ok_or(SHAPE),
        None => {
            let text = |name: &str| package.get(name).and_then(Document::text).ok_or(SHAPE);
            Ok(format!("{}@{}", text("name")?, text("version")?))
        }
    }
}

/// What the image's `policy` says of the licences of `packages`, with the
/// reviews among `exceptions` that waive a finding.
pub fn license_verdict(
    packages: &[Document],
    policy: &Document,
    exceptions: &[Exception],
) -> Result<Document> {
    let allowed = policy
        .get("allowedLicenses")
        .and_then(Document::items)
        .ok_or(SHAPE)?;
    let allowed: Option<Vec<&str>> = allowed.iter().map(Document::text).collect();
    let allowed = Allowed::new(allowed.ok_or(SHAPE)?);
    let (mut blocked, mut waived) = (Vec::new(), Vec::new());
    for package in packages {
        let records = package
            .get("licenses")
            .and_then(Document::items)
            .ok_or(SHAPE)?;
        let mut expressions = Vec::with_capacity(records.len());
        for record in records {
            require(record.fields().is_some(), SHAPE.0)?;
            let expression = record
                .get("spdxExpression")
                .map_or(Some(""), Document::text);
            expressions.push(expression.ok_or(SHAPE)?);
        }
        let stated = expressions
            .iter()
            .filter(|expression| !expression.is_empty());
        let expression: Vec<String> = stated.map(|expression| format!("({expression})")).collect();
        let expression = expression.join(" AND ");
        let complete =
            !expressions.is_empty() && expressions.iter().all(|expression| !expression.is_empty());
        // An expression is judged only when every record has one, and then
        // each record's must be a whole expression by itself as well: in
        // its parentheses, one that closed them and opened others, such as
        // `X) OR (MIT`, would make the conjunction of the records a choice.
        // The whole is judged first, so that a record that is no expression
        // is refused as the reference refused it.
        let mut approved = complete && {
            let whole = permits(&expression, &allowed)?;
            for alone in &expressions {
                permits(alone, &allowed)?;
            }
            whole
        };
        let application = package.get("name") == Some(&"simplestchat".into())
            && package.get("type") == Some(&"rust-crate".into());
        if application {
            approved = expressions == ["NOASSERTION"];
        }
        if approved {
            continue;
        }
        let scope = scope(package)?;
        let raw = if complete {
            None
        } else {
            Some(raw_license_identity(records)?)
        };
        let fingerprint = match &raw {
            None => format!("license:{}", digest(expression.as_bytes())),
            Some(raw) => raw.fingerprint.clone(),
        };
        let shown = if expression.is_empty() {
            "UNKNOWN"
        } else {
            &expression
        };
        let mut finding = vec![
            ("scope".to_owned(), Document::from(scope.as_str())),
            ("expression".to_owned(), shown.into()),
            ("fingerprint".to_owned(), fingerprint.as_str().into()),
        ];
        if let Some(raw) = raw {
            finding.push((
                "rawLicenseRecords".to_owned(),
                Document::List(records.to_vec()),
            ));
            finding.push((
                "rawLicenseRecordsSha256".to_owned(),
                raw.records_sha256.into(),
            ));
        }
        if permitted(exceptions, "image-license", &fingerprint, &scope) {
            waived.push(Document::Record(finding));
        } else {
            blocked.push(Document::Record(finding));
        }
    }
    Ok(Document::record([
        ("passed", blocked.is_empty().into()),
        ("packages", packages.len().into()),
        ("blocked", Document::List(blocked)),
        ("waived", Document::List(waived)),
    ]))
}

#[cfg(test)]
#[path = "licenses_tests.rs"]
mod tests;
