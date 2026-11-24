//! GitHub's stored analyses and alerts held to the reviews: the four
//! automated analyses current and healthy at the exact reference, every
//! active first-party alert of them assessed, and a reviewed false
//! positive dismissed only once its plan, its alert, the reference, the
//! policy and the reviews are each read again unchanged.

use super::{
    Ledger, MAX_APPLY, MAX_POLICY, NO_ITEMS, among, array, bounded_file, decode, digest, field,
    findings::finding, findings::verdict, get, get_or, github::Github, holds, object, positive,
    same, string,
};
use crate::{
    Result,
    codeql::{
        alike,
        sources::{NativeSources, Shown},
    },
    commit,
    document::Document,
    policy::{Exception, permitted},
    require,
};
use std::{collections::BTreeSet, path::Path};

/// Where a finding's source is read: the checkout `root` at `revision`,
/// its native sources through `sources`, and what was committed through
/// `git`.
pub struct Checkout<'a, 'b> {
    /// The checkout.
    pub root: &'a Path,
    /// The native sources, when a cache binds them.
    pub sources: Option<&'a mut NativeSources<'b>>,
    /// What was committed.
    pub git: &'a dyn Shown,
}

/// The finding of the alert `raw` at the exact `reference` and `revision`,
/// with its number, its severity and its state on GitHub.
pub fn api_finding(
    raw: &Document,
    checkout: &mut Checkout<'_, '_>,
    revision: &str,
    reference: &str,
) -> Result<Document> {
    let tool = object(field(raw, "tool")?)?;
    require(same(field(tool, "name")?, "CodeQL"), "codeql_api_tool")?;
    let instance = object(field(raw, "most_recent_instance")?)?;
    require(
        same(field(instance, "ref")?, reference) && same(field(instance, "commit_sha")?, revision),
        "codeql_stale_alert",
    )?;
    require(
        among(field(instance, "state")?, &["open", "dismissed"])?,
        "codeql_instance_state",
    )?;
    let rule = object(field(raw, "rule")?)?;
    let mut severity = get(rule, "security_severity_level").clone();
    if severity == Document::Null {
        let tags = array(get_or(rule, "tags", &NO_ITEMS))?;
        require(!holds(tags, "security"), "codeql_missing_score")?;
        severity = "advisory".into();
    }
    require(
        among(&severity, &super::SEVERITIES)?,
        "codeql_unknown_severity",
    )?;
    let identifier = string(field(rule, "id")?)?;
    let version = string(field(tool, "version")?)?;
    let location = object(field(instance, "location")?)?;
    let message = string(field(object(field(instance, "message")?)?, "text")?)?;
    let sources = checkout.sources.as_deref_mut();
    let mut found = finding(
        checkout.root,
        revision,
        identifier,
        version,
        location,
        message,
        sources,
        checkout.git,
    )?;
    found.put("number", positive(field(raw, "number")?)?.into());
    found.put("severity", severity);
    found.put("state", field(raw, "state")?.clone());
    Ok(found)
}

/// The four automated security analyses stored for the exact `reference`
/// at `revision`, each successful and nonempty, the reference at
/// `revision` before and after they are read.
pub fn analysis_health(
    client: &mut Github<'_>,
    revision: &str,
    reference: &str,
) -> Result<Document> {
    require(commit(revision), "codeql_revision")?;
    require(
        client.head(reference)? == revision,
        "codeql_ref_head_changed",
    )?;
    let analyses = client.pages("analyses", reference, Some(revision))?;
    let expected = client.categories.clone();
    let mut categories = BTreeSet::new();
    let mut records = Vec::new();
    for item in &analyses {
        let category = string(field(item, "category")?)?.trim_end_matches('/');
        require(
            expected.iter().any(|held| held == category) && !categories.contains(category),
            "codeql_analysis_category",
        )?;
        require(
            same(field(item, "ref")?, reference)
                && same(field(item, "commit_sha")?, revision)
                && same(field(object(field(item, "tool")?)?, "name")?, "CodeQL"),
            "codeql_analysis_identity",
        )?;
        require(
            same(field(item, "error")?, "") && same(field(item, "warning")?, ""),
            "codeql_analysis_failed",
        )?;
        require(
            positive(field(item, "rules_count")?)? > 0,
            "codeql_analysis_empty",
        )?;
        records.push(Document::record([
            ("id", positive(field(item, "id")?)?.into()),
            ("category", category.into()),
            ("rulesCount", field(item, "rules_count")?.clone()),
        ]));
        categories.insert(category.to_owned());
    }
    let every: BTreeSet<String> = expected.into_iter().collect();
    require(categories == every, "codeql_missing_current_analysis")?;
    require(
        client.head(reference)? == revision,
        "codeql_ref_head_changed",
    )?;
    let sorted: Vec<Document> = categories.into_iter().map(Document::from).collect();
    Ok(Document::record([
        ("schemaVersion", 1_u64.into()),
        ("passed", true.into()),
        ("repository", client.repository.as_str().into()),
        ("revision", revision.into()),
        ("ref", reference.into()),
        ("refHead", revision.into()),
        ("analysisCategories", sorted.into()),
        ("analyses", records.into()),
    ]))
}

/// The SHA-256 of the exact bytes of the policy, which stay fixed through a
/// mutating operation.
pub fn policy_identity(ledger: &Ledger) -> Result<String> {
    Ok(crate::digest(&bounded_file(&ledger.policy, MAX_POLICY)?))
}

/// The hash of both explicit false-positive reviews, by their names.
pub fn review_identity(ledger: &Ledger) -> Result<String> {
    let mut hashes = Vec::new();
    for path in &ledger.reviews {
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let hash = crate::digest(&bounded_file(path, MAX_POLICY)?);
        hashes.push((name.into_owned(), Document::from(hash)));
    }
    Ok(digest(&Document::Record(hashes)))
}

/// Whether one review of `repository`, and only one, decides that the
/// finding `item` of its number is a false positive of its exact
/// fingerprint and scope. A risk acceptance or another repository's review
/// never is.
pub fn false_positive(item: &Document, repository: &str, ledger: &Ledger) -> Result<bool> {
    let mut matches = Vec::new();
    for path in &ledger.reviews {
        let raw = decode(&bounded_file(path, MAX_POLICY)?)?;
        let raw = object(&raw)?;
        let schema = alike(field(raw, "schemaVersion")?, &Document::from(1_u64));
        require(schema, "codeql_review_schema")?;
        if same(field(raw, "repository")?, repository) {
            for entry in array(field(raw, "alerts")?)? {
                let entry = object(entry)?;
                if alike(get(entry, "number"), get(item, "number")) {
                    matches.push(entry.clone());
                }
            }
        }
    }
    require(matches.len() <= 1, "codeql_duplicate_review")?;
    Ok(matches.first().is_some_and(|decided| {
        same(get(decided, "disposition"), "false-positive")
            && alike(get(decided, "fingerprint"), get(item, "fingerprint"))
            && alike(get(decided, "scope"), get(item, "scope"))
    }))
}

/// The assessment of every active first-party alert of the automated
/// security categories at the exact `reference` and `revision`, held to the
/// reviews `reviews` and bound to the ledger's exact bytes.
pub fn api_plan(
    client: &mut Github<'_>,
    checkout: &mut Checkout<'_, '_>,
    revision: &str,
    reference: &str,
    reviews: &[Exception],
    ledger: &Ledger,
) -> Result<Document> {
    let health = analysis_health(client, revision, reference)?;
    let expected = client.categories.clone();
    let mut findings = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in client.pages("alerts", reference, None)? {
        require(
            client.clock().now() < client.deadline,
            "codeql_api_deadline",
        )?;
        let number = positive(field(&raw, "number")?)?;
        require(!seen.contains(&number), "codeql_duplicate_alert")?;
        seen.insert(number);
        require(
            among(field(&raw, "state")?, &["open", "dismissed", "fixed"])?,
            "codeql_alert_state",
        )?;
        let instance = object(field(&raw, "most_recent_instance")?)?;
        require(same(field(instance, "ref")?, reference), "codeql_alert_ref")?;
        if same(field(instance, "state")?, "fixed") {
            continue;
        }
        let category = string(field(instance, "category")?)?.trim_end_matches('/');
        if !expected.iter().any(|held| held == category)
            || string(field(object(field(instance, "location")?)?, "path")?)?.starts_with("vendor/")
        {
            continue;
        }
        findings.push(api_finding(&raw, checkout, revision, reference)?);
    }
    require(
        client.head(reference)? == revision,
        "codeql_ref_head_changed",
    )?;
    let review = review_identity(ledger)?;
    let categories = field(&health, "analysisCategories")?.clone();
    let judged = verdict(&findings, reviews)?;
    let mut plan = Document::record([
        ("schemaVersion", 1_u64.into()),
        ("repository", client.repository.as_str().into()),
        ("revision", revision.into()),
        ("ref", reference.into()),
        ("refHead", revision.into()),
        ("reviewSha256", review.into()),
        ("analysisCategories", categories),
    ]);
    for (name, value) in judged.fields().unwrap_or_default() {
        plan.put(name, value.clone());
    }
    plan.put("policySha256", policy_identity(ledger)?.into());
    Ok(plan)
}

/// Record a step of an application before the next one begins.
pub type Journal<'a> = dyn FnMut(&Document) -> Result<()> + 'a;

/// Dismiss each open waived alert of the plan `plan` as a false positive,
/// once the plan is made again unchanged and the alert, the reference, the
/// policy and the reviews are each read again unchanged just before its
/// write; each write is journalled before it is made and once it is
/// confirmed.
pub fn apply_plan(
    client: &mut Github<'_>,
    checkout: &mut Checkout<'_, '_>,
    plan: &Document,
    reviews: &[Exception],
    journal: &mut Journal<'_>,
    ledger: &Ledger,
) -> Result<Document> {
    let repository = client.repository.clone();
    require(
        same(field(plan, "repository")?, &repository),
        "codeql_apply_repository",
    )?;
    let revision = string(field(plan, "revision")?)?;
    let reference = string(field(plan, "ref")?)?;
    let fresh = api_plan(client, checkout, revision, reference, reviews, ledger)?;
    require(alike(&fresh, plan), "codeql_apply_plan_changed")?;
    let mut selected = Vec::new();
    for item in array(field(plan, "waived")?)? {
        if same(field(object(item)?, "state")?, "open") {
            selected.push(object(item)?);
        }
    }
    require(selected.len() <= MAX_APPLY, "codeql_apply_limit")?;
    let mut decided = true;
    for item in &selected {
        if !false_positive(item, &repository, ledger)? {
            decided = false;
            break;
        }
    }
    require(decided, "codeql_not_false_positive")?;
    let mut completed: Vec<u64> = Vec::new();
    let listed = |numbers: &[u64]| Document::List(numbers.iter().map(|n| (*n).into()).collect());
    for expected in selected {
        client.prepare_write()?;
        let number = positive(field(expected, "number")?)?;
        let current = client.request(&format!("alerts/{number}"), None)?;
        let current = api_finding(object(&current)?, checkout, revision, reference)?;
        require(alike(&current, expected), "codeql_apply_alert_changed")?;
        require(
            client.head(reference)? == revision,
            "codeql_ref_head_changed",
        )?;
        let unchanged = {
            let policy = policy_identity(ledger)?;
            same(field(plan, "policySha256")?, &policy)
        } && {
            let review = review_identity(ledger)?;
            same(field(plan, "reviewSha256")?, &review)
        } && false_positive(expected, &repository, ledger)?
            && permitted(
                reviews,
                "codeql",
                string(field(expected, "fingerprint")?)?,
                string(field(expected, "scope")?)?,
            );
        require(unchanged, "codeql_apply_review_changed")?;
        let comment = format!(
            "Reviewed false positive; {}; policy {}",
            string(field(expected, "fingerprint")?)?,
            string(field(plan, "policySha256")?)?,
        );
        journal(&Document::record([
            ("phase", "dismissal_requested".into()),
            ("number", number.into()),
            ("dismissed", listed(&completed)),
        ]))?;
        let body = Document::record([
            ("state", "dismissed".into()),
            ("dismissed_reason", "false positive".into()),
            ("dismissed_comment", comment.as_str().into()),
        ]);
        let result = client.request(&format!("alerts/{number}"), Some(&body))?;
        let result = object(&result)?;
        let mut dismissed = expected.clone();
        dismissed.put("state", "dismissed".into());
        let confirmed = alike(field(result, "number")?, &Document::from(number))
            && same(field(result, "state")?, "dismissed")
            && same(field(result, "dismissed_reason")?, "false positive")
            && same(field(result, "dismissed_comment")?, &comment)
            && alike(
                &api_finding(result, checkout, revision, reference)?,
                &dismissed,
            );
        require(confirmed, "codeql_dismissal_unconfirmed")?;
        require(
            client.head(reference)? == revision,
            "codeql_ref_head_changed",
        )?;
        require(
            client.clock().now() < client.deadline,
            "codeql_api_deadline",
        )?;
        completed.push(number);
        journal(&Document::record([
            ("phase", "dismissal_confirmed".into()),
            ("number", number.into()),
            ("dismissed", listed(&completed)),
        ]))?;
    }
    Ok(Document::record([
        ("schemaVersion", 1_u64.into()),
        ("repository", repository.into()),
        ("revision", revision.into()),
        ("dismissed", listed(&completed)),
    ]))
}
