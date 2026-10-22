//! Fixed-repository API reads and refreshed, bounded exact-ID cache deletion.

use super::{
    policy::{self, Entry, MAX_CACHES, MAX_DELETIONS, PAGE_SIZE, REFERENCE, REPOSITORY},
    process,
};
use crate::tooling::Environment;
use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub(super) struct Github {
    root: PathBuf,
    environment: Environment,
    pub apply: bool,
    pub deadline: Instant,
    pub deleted: Vec<u64>,
    pub now: fn() -> DateTime<Utc>,
    pub per_request: Duration,
    signals: process::Signals,
    cancelled: bool,
    authorized_revision: Option<String>,
}

impl Github {
    pub(super) fn new(
        root: PathBuf,
        mut environment: Environment,
        apply: bool,
        signals: process::Signals,
    ) -> Self {
        environment.remove(OsStr::new("GH_DEBUG"));
        for (name, value) in [
            ("GH_PROMPT_DISABLED", "1"),
            ("GH_HOST", "github.com"),
            ("GH_PAGER", "cat"),
        ] {
            environment.insert(name.into(), value.into());
        }
        Self {
            root,
            environment,
            apply,
            deadline: Instant::now() + Duration::from_secs(240),
            deleted: Vec::new(),
            now: Utc::now,
            per_request: Duration::from_secs(20),
            signals,
            cancelled: false,
            authorized_revision: None,
        }
    }

    pub(super) async fn request(&mut self, suffix: &str, delete: bool) -> Result<Value> {
        policy::require(
            !self.cancelled && allowed_endpoint(suffix, delete, self.apply, self.deleted.len()),
            "cache_retention_endpoint",
        )?;
        policy::require(
            !delete || self.authorized_revision.is_some(),
            "cache_retention_untrusted_run",
        )?;
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        policy::require(!remaining.is_zero(), "cache_retention_deadline")?;
        let arguments = [
            "api",
            "--hostname",
            "github.com",
            "--method",
            if delete { "DELETE" } else { "GET" },
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "X-GitHub-Api-Version: 2022-11-28",
        ]
        .into_iter()
        .map(str::to_owned)
        .chain([format!("repos/{REPOSITORY}/{suffix}")])
        .collect::<Vec<_>>();
        let result = process::capture(
            &self.root,
            &self.environment,
            &arguments,
            self.per_request.min(remaining),
            &mut self.signals,
        )
        .await;
        let bytes = match result {
            Ok(bytes) => bytes,
            Err(error) => {
                if error.downcast_ref::<process::Interrupted>().is_some() {
                    self.cancelled = true;
                }
                return Err(error);
            }
        };
        if delete {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("cache_retention_json"))
    }

    pub(super) async fn inventory(&mut self, key: Option<&str>) -> Result<Vec<Entry>> {
        let mut entries = Vec::new();
        let mut total = None;
        let mut identifiers = BTreeSet::new();
        for page in 1..=MAX_CACHES / PAGE_SIZE {
            let mut query = url::form_urlencoded::Serializer::new(String::new());
            query
                .append_pair("ref", REFERENCE)
                .append_pair("per_page", &PAGE_SIZE.to_string())
                .append_pair("page", &page.to_string())
                .append_pair("sort", "created_at")
                .append_pair("direction", "desc");
            if let Some(key) = key {
                query.append_pair("key", key);
            }
            let raw = self
                .request(&format!("actions/caches?{}", query.finish()), false)
                .await?;
            let current = raw["total_count"]
                .as_u64()
                .ok_or_else(|| anyhow::anyhow!("cache_retention_count"))?;
            policy::require(
                current <= MAX_CACHES as u64 && total.is_none_or(|count| count == current),
                "cache_retention_count",
            )?;
            total = Some(current);
            let batch = raw["actions_caches"]
                .as_array()
                .ok_or_else(|| anyhow::anyhow!("cache_retention_page"))?;
            policy::require(batch.len() <= PAGE_SIZE, "cache_retention_page")?;
            for item in batch {
                let item = Entry::parse(item)?;
                policy::require(identifiers.insert(item.id), "cache_retention_duplicate")?;
                entries.push(item);
            }
            if entries.len() as u64 == current {
                return Ok(entries);
            }
            policy::require(
                !batch.is_empty() && (entries.len() as u64) < current,
                "cache_retention_incomplete",
            )?;
        }
        bail!("cache_retention_pages")
    }

    pub(super) async fn trusted_run(&mut self, run_id: u64, revision: &str) -> Result<()> {
        self.authorized_revision = None;
        policy::require(
            run_id > 0 && policy::digest(revision, 40),
            "cache_retention_run",
        )?;
        let run = self
            .request(&format!("actions/runs/{run_id}"), false)
            .await?;
        policy::require(
            run["id"].as_u64() == Some(run_id)
                && run["name"] == "CI"
                && run["event"] == "push"
                && run["status"] == "completed"
                && run["conclusion"] == "success"
                && run["head_branch"] == "main"
                && run["head_sha"] == revision
                && run["path"] == ".github/workflows/ci.yml"
                && run["repository"]["full_name"] == REPOSITORY
                && run["head_repository"]["full_name"] == REPOSITORY,
            "cache_retention_untrusted_run",
        )?;
        self.current_head(revision).await?;
        self.authorized_revision = Some(revision.into());
        Ok(())
    }

    pub(super) async fn current_head(&mut self, revision: &str) -> Result<()> {
        let reference = self.request("git/ref/heads/main", false).await?;
        policy::require(
            reference["object"]["sha"] == revision,
            "cache_retention_stale_run",
        )
    }

    pub(super) async fn remove(&mut self, entry: &Entry, revision: &str) -> Result<bool> {
        policy::require(
            self.apply && entry.reference == REFERENCE && policy::family(&entry.key).is_some(),
            "cache_retention_apply",
        )?;
        policy::require(
            self.authorized_revision.as_deref() == Some(revision),
            "cache_retention_untrusted_run",
        )?;
        self.current_head(revision).await?;
        let refreshed = self.inventory(None).await?;
        if !policy::plan(&refreshed, (self.now)())
            .iter()
            .any(|item| item.id == entry.id)
        {
            return Ok(false);
        }
        let Some(current) = refreshed.iter().find(|item| item.id == entry.id) else {
            return Ok(false);
        };
        policy::require(entry.unchanged(current), "cache_retention_changed")?;
        if current.recent((self.now)()) {
            return Ok(false);
        }
        self.request(&format!("actions/caches/{}", current.id), true)
            .await?;
        self.deleted.push(current.id);
        Ok(true)
    }
}

fn decimal(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit())
}

pub(super) fn allowed_endpoint(suffix: &str, delete: bool, apply: bool, deleted: usize) -> bool {
    if delete {
        apply
            && deleted < MAX_DELETIONS
            && suffix.strip_prefix("actions/caches/").is_some_and(decimal)
    } else {
        suffix == "git/ref/heads/main"
            || suffix.starts_with("actions/caches?")
            || suffix.strip_prefix("actions/runs/").is_some_and(decimal)
    }
}
