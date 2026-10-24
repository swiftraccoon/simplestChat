//! Parse real workflow YAML and require the original first-party coverage policy.

use super::files;
use anyhow::{Result, ensure};
use serde::Deserialize;
use serde_json::{Map, Value};
use serde_saphyr::{DuplicateKeyPolicy, MergeKeyPolicy};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

const LANGUAGES: [&str; 4] = ["actions", "javascript-typescript", "python", "rust"];
pub(super) const NAMES: [&str; 3] = ["ci.yml", "security.yml", "codeql.yml"];

#[derive(Deserialize)]
struct Workflow {
    jobs: BTreeMap<String, Value>,
}

/// Parse only workflow jobs; ignored keys still pass through the bounded parser.
/// Last-wins duplicates and YAML 1.1 merge keys match PyYAML's safe loader.
pub(super) fn jobs(bytes: &[u8]) -> Result<BTreeMap<String, Value>> {
    let options = serde_saphyr::options! {
        duplicate_keys: DuplicateKeyPolicy::LastWins,
        merge_keys: MergeKeyPolicy::Merge,
        strict_booleans: false,
        reject_unsupported_tags: true,
        budget: serde_saphyr::budget! {
            max_reader_input_bytes: Some(files::MAX_DOCUMENT as usize),
            max_events: 100_000,
            max_nodes: 50_000,
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
        },
        alias_limits: serde_saphyr::alias_limits! {
            max_total_replayed_events: 100_000,
            max_replay_stack_depth: 32,
            max_alias_expansions_per_anchor: 1_024,
        },
    };
    Ok(serde_saphyr::from_slice_with_options::<Workflow>(bytes, options)?.jobs)
}

pub(super) fn object(value: &Value) -> Result<&Map<String, Value>> {
    value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("scanner_object_expected"))
}

fn string(value: &Value) -> Result<&str> {
    value
        .as_str()
        .filter(|value| !value.is_empty() && value.chars().count() <= 2048)
        .ok_or_else(|| anyhow::anyhow!("invalid_tool_string"))
}

fn strings(value: &Value) -> Result<Vec<&str>> {
    value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("scanner_list_expected"))?
        .iter()
        .map(string)
        .collect()
}

fn member<'a>(object: &'a Map<String, Value>, name: &str) -> Result<&'a Value> {
    object.get(name).ok_or_else(|| anyhow::anyhow!("'{name}'"))
}

fn matrix(job: &Value) -> Result<&Map<String, Value>> {
    object(member(
        object(member(object(job)?, "strategy")?)?,
        "matrix",
    )?)
}

fn job<'a>(jobs: &'a BTreeMap<String, Value>, name: &str) -> Result<&'a Value> {
    jobs.get(name).ok_or_else(|| anyhow::anyhow!("'{name}'"))
}

pub(super) struct Inventory {
    pub(super) gates: BTreeSet<String>,
    pub(super) checks: BTreeSet<String>,
    pub(super) hashes: BTreeMap<String, String>,
}

impl Inventory {
    pub(super) fn read(root: &Path) -> Result<Self> {
        let mut workflows = BTreeMap::new();
        let mut hashes = BTreeMap::new();
        for name in NAMES {
            let bytes = files::bounded(&root.join(".github/workflows").join(name))?;
            let hash = Sha256::digest(&bytes)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            hashes.insert(name.to_owned(), hash);
            workflows.insert(name, jobs(&bytes)?);
        }
        let ci = &workflows["ci.yml"];
        let security = &workflows["security.yml"];
        let codeql = &workflows["codeql.yml"];
        let gates = strings(member(object(job(ci, "required")?)?, "needs")?)?
            .into_iter()
            .map(str::to_owned)
            .collect();
        let references = matrix(job(ci, "automation-references")?)?;
        ensure!(
            references.keys().map(String::as_str).collect::<Vec<_>>() == ["group"],
            "local_ci_reference_matrix_shape"
        );
        let reference_groups = strings(member(references, "group")?)?;
        ensure!(
            reference_groups.iter().copied().collect::<BTreeSet<_>>()
                == BTreeSet::from(["foundation", "fetch", "host", "deploy"])
                && reference_groups.len() == 4,
            "local_ci_reference_coverage"
        );
        let browser = matrix(job(ci, "browser")?)?;
        ensure!(
            browser.keys().map(String::as_str).collect::<BTreeSet<_>>()
                == BTreeSet::from(["group", "include"]),
            "local_ci_browser_matrix_shape"
        );
        let mut groups = strings(member(browser, "group")?)?;
        let included = member(browser, "include")?
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("scanner_list_expected"))?;
        let mut included = included
            .iter()
            .map(|value| {
                let value = member(object(value)?, "group")?;
                Ok(string(value)?.to_owned())
            })
            .collect::<Result<Vec<_>>>()?;
        groups.sort_unstable();
        included.sort_unstable();
        ensure!(groups == included, "local_ci_browser_matrix_ports");
        ensure!(
            !security.contains_key("native-security") && !codeql.contains_key("native-analysis"),
            "local_ci_vendor_scan_is_optional"
        );
        let source = matrix(job(codeql, "source-analysis")?)?;
        ensure!(
            source.keys().map(String::as_str).collect::<Vec<_>>() == ["language"],
            "local_ci_codeql_matrix_shape"
        );
        let languages = strings(member(source, "language")?)?;
        let pins = files::document(&root.join("security/codeql-toolchain.json"))?;
        let pinned = object(&pins["languages"])?;
        ensure!(
            languages.iter().copied().collect::<BTreeSet<_>>() == BTreeSet::from(LANGUAGES)
                && languages
                    .iter()
                    .all(|language| pinned.contains_key(*language))
                && languages.len() == LANGUAGES.len(),
            "local_ci_codeql_coverage"
        );
        ensure!(
            !groups.is_empty()
                && groups.len() == groups.iter().copied().collect::<BTreeSet<_>>().len(),
            "local_ci_browser_coverage"
        );
        let checks = groups
            .into_iter()
            .map(|group| format!("browser-{group}"))
            .chain(
                languages
                    .into_iter()
                    .map(|language| format!("codeql-{language}")),
            )
            .chain(
                reference_groups
                    .into_iter()
                    .map(|group| format!("reference-{group}")),
            )
            .chain(["security-fast".to_owned()])
            .collect();
        Ok(Self {
            gates,
            checks,
            hashes,
        })
    }
}
