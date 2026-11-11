//! Import bounded public GitHub check evidence: no credentials, no arbitrary
//! destination. Two fixed workflows of one explicitly configured repository
//! are read through a total request budget, each attempt is classified from
//! its actual job and steps, and snapshots reach the database through a
//! bounded private spool that survives outages.

use super::alerts;
use super::collect;
use super::report::database;
use super::text::float;
use crate::host::Host;
use crate::host::command::Request;
use crate::host::failure::{Failure, Result, require};
use crate::host::state;
use crate::json::Json;
use chrono::{DateTime, NaiveDateTime, Utc};
use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt;

const MAX_REQUESTS: usize = 8;
const MAX_RESPONSE: usize = 4 * 1024 * 1024;
const MAX_SPOOL: usize = 144;
const MAX_SNAPSHOT: usize = 120 * 1024;
const INTERVAL: f64 = 600.0;
const MAX_ATTEMPTS: i128 = 10;
const MAX_ATTEMPT_ID: i128 = 1_000_000;
const PAGE_SIZE: usize = 100;
const MAX_RUNS: i128 = 200;
const MAX_JOBS: usize = 4;
const MAX_STEPS: usize = 40;
const MAX_SEEN: usize = 4000;
const MAX_ORIGIN_LENGTH: usize = 261;
const CONCLUSIONS: [&str; 9] = [
    "success",
    "failure",
    "cancelled",
    "timed_out",
    "skipped",
    "neutral",
    "action_required",
    "stale",
    "unknown",
];

/// One fixed workflow: its file stem, the one job it must have and the steps
/// that are its checks.
struct Workflow {
    name: &'static str,
    job: &'static str,
    steps: &'static [(&'static str, &'static str)],
}

const WORKFLOWS: [Workflow; 2] = [
    Workflow {
        name: "availability",
        job: "External HTTPS readiness",
        steps: &[(
            "readiness",
            "Require configured origin and check readiness through the public proxy",
        )],
    },
    Workflow {
        name: "canary",
        job: "Owned browsers against the public deployment",
        steps: &[
            ("direct", "Publish and receive in the canary room"),
            ("relay", "Require decoded media through TURN"),
        ],
    },
];
const COVERAGE: &str = "BEGIN READ ONLY; SET LOCAL statement_timeout='3s'; \
SELECT COALESCE(json_agg(json_build_object('workflow',s.workflow, \
'failed',s.failure_since IS NOT NULL,'conclusive',COALESCE( \
(SELECT r.result IN ('success','failure') FROM operations.external_runs r \
WHERE r.workflow=s.workflow ORDER BY completed_at DESC,run_id DESC,attempt DESC \
LIMIT 1),false),'latest',COALESCE( \
(SELECT extract(epoch FROM max(completed_at)) FROM operations.external_runs r \
WHERE r.workflow=s.workflow AND r.result IN ('success','failure')),0))), '[]'::json) \
FROM operations.external_status s; COMMIT;";

/// The explicit public evidence source, bound to the deployment's origin.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
struct Deployment {
    repository: String,
    origin: String,
}

impl Deployment {
    /// Keep GitHub paths and public origins finite and free of credentials.
    fn new(repository: &str, origin: &str) -> Result<Self> {
        let part = |text: &str, most: usize, extra: &[u8]| {
            (1..=most).contains(&text.len())
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || extra.contains(&byte))
        };
        let named = repository
            .split_once('/')
            .is_some_and(|(owner, name)| part(owner, 39, b"-") && part(name, 100, b"_.-"));
        let explicit = "Configure an explicit public GitHub repository for external monitoring";
        require(named, explicit)?;
        let public = origin.len() <= MAX_ORIGIN_LENGTH
            && origin.strip_prefix("https://").is_some_and(collect::domain);
        require(public, "Invalid external monitoring origin")?;
        Ok(Self {
            repository: repository.to_owned(),
            origin: origin.to_owned(),
        })
    }
}

/// Read only the root-owned monitoring settings; there is no default source.
fn configured(host: &Host<'_>) -> Result<Deployment> {
    state::protected(
        &host.paths.settings,
        host.owner,
        false,
        &[0o600],
        Some(16384),
    )?;
    let settings = state::decode(&std::fs::read(&host.paths.settings)?)?;
    settings.object().ok_or(Failure::Value)?;
    let repository = match settings.get("external_repository") {
        None => "",
        Some(value) => value.string().ok_or(Failure::Value)?,
    };
    let domain = settings.get("domain").ok_or(Failure::Key)?;
    let origin = format!("https://{}", domain.string().ok_or(Failure::Value)?);
    Deployment::new(repository, &origin)
}

/// A whole number that is spelled as one.
fn whole(value: Option<&Json>) -> Result<i128> {
    match value.ok_or(Failure::Key)? {
        Json::Integer(text) => text.parse().map_err(|_| Failure::Value),
        _ => Err(Failure::Value),
    }
}

fn text(value: Option<&Json>) -> Result<&str> {
    value.ok_or(Failure::Key)?.string().ok_or(Failure::Value)
}

fn object(value: Option<&Json>) -> Result<&Json> {
    let value = value.ok_or(Failure::Key)?;
    value.object().map(|_| value).ok_or(Failure::Value)
}

fn array(value: Option<&Json>) -> Result<&[Json]> {
    value.ok_or(Failure::Key)?.array().ok_or(Failure::Value)
}

/// A number, or text that spells one, as a float.
fn loose(value: Option<&Json>) -> Result<f64> {
    match value {
        None => Ok(0.0),
        Some(number @ (Json::Integer(_) | Json::Float(_))) => number.number().ok_or(Failure::Value),
        Some(Json::String(text)) => text.trim().parse().map_err(|_| Failure::Value),
        Some(_) => Err(Failure::Value),
    }
}

/// The importer's durable state beside its spool.
#[derive(serde::Serialize)]
struct State {
    #[serde(skip_serializing_if = "Option::is_none")]
    deployment: Option<Deployment>,
    #[serde(rename = "lastPollAt")]
    last_poll_at: f64,
    #[serde(rename = "observedAt", skip_serializing_if = "Option::is_none")]
    observed_at: Option<String>,
    seen: BTreeMap<String, Seen>,
    #[serde(rename = "notBefore", skip_serializing_if = "Option::is_none")]
    not_before: Option<f64>,
    dropped: i128,
}

/// When a remembered attempt's run was created, for expiry.
#[derive(serde::Serialize)]
struct Seen {
    #[serde(rename = "createdAt")]
    created_at: String,
}

/// An instant as an ISO text with its offset, with microseconds only when
/// there are any.
fn iso(instant: DateTime<Utc>) -> String {
    if instant.timestamp_subsec_micros() == 0 {
        instant.format("%Y-%m-%dT%H:%M:%S+00:00").to_string()
    } else {
        instant.format("%Y-%m-%dT%H:%M:%S%.6f+00:00").to_string()
    }
}

/// Seconds since the epoch, to the microsecond.
fn seconds(instant: DateTime<Utc>) -> f64 {
    #[expect(
        clippy::cast_precision_loss,
        reason = "microseconds of this era fit a double"
    )]
    let micros = instant.timestamp_micros() as f64;
    micros / 1_000_000.0
}

/// Accept only GitHub's explicit UTC timestamp spelling.
fn instant(value: Option<&Json>) -> Result<DateTime<Utc>> {
    let spelled = value.and_then(Json::string).filter(|text| {
        let bytes = text.as_bytes();
        bytes.len() == 20
            && bytes.iter().enumerate().all(|(at, byte)| match at {
                4 | 7 => *byte == b'-',
                10 => *byte == b'T',
                13 | 16 => *byte == b':',
                19 => *byte == b'Z',
                _ => byte.is_ascii_digit(),
            })
    });
    require(spelled.is_some(), "Invalid external timestamp")?;
    let parsed = NaiveDateTime::parse_from_str(spelled.unwrap_or_default(), "%Y-%m-%dT%H:%M:%SZ");
    Ok(parsed.map_err(|_| Failure::Value)?.and_utc())
}

/// The fixed unauthenticated API under a total request budget.
struct Github<'a> {
    host: &'a Host<'a>,
    deployment: &'a Deployment,
    requests: usize,
    not_before: Option<f64>,
}

impl Github<'_> {
    /// Whether `path` is one of the two internally constructed shapes.
    fn allowed(path: &str) -> bool {
        let digits = |text: &str| {
            !text.is_empty()
                && !text.starts_with('0')
                && text.bytes().all(|byte| byte.is_ascii_digit())
        };
        let listing = |workflow: &str| {
            let prefix = format!("workflows/{workflow}.yml/runs?");
            path.strip_prefix(prefix.as_str()).is_some_and(|query| {
                !query.is_empty()
                    && query
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"%=&_.+-".contains(&byte))
            })
        };
        let jobs = || {
            let rest = path.strip_prefix("runs/")?;
            let (run, rest) = rest.split_once("/attempts/")?;
            let attempt = rest.strip_suffix("/jobs?per_page=100")?;
            Some(digits(run) && digits(attempt))
        };
        listing("availability") || listing("canary") || jobs() == Some(true)
    }

    /// Fetch one path. Redirects are refused, and a rate limit defers every
    /// later request for at least the polling interval.
    fn get(&mut self, path: &str) -> Result<Json> {
        require(
            self.requests < MAX_REQUESTS,
            "GitHub polling budget exhausted",
        )?;
        require(Self::allowed(path), "Unexpected GitHub API path")?;
        let now = seconds(self.host.clock.now());
        require(
            now >= self.not_before.unwrap_or(0.0),
            "GitHub retry deferred",
        )?;
        self.requests += 1;
        let url = format!(
            "https://api.github.com/repos/{}/actions/{path}",
            self.deployment.repository
        );
        let argv = [
            "/usr/bin/curl",
            "--disable",
            "--silent",
            "--show-error",
            "--noproxy",
            "*",
            "--max-redirs",
            "0",
            "--proto",
            "=https",
            "--max-time",
            "3",
            "--header",
            "Accept: application/vnd.github+json",
            "--header",
            "X-GitHub-Api-Version: 2026-03-10",
            "--header",
            "User-Agent: simplestchat-private-monitor/1",
            "--output",
            "-",
            "--write-out",
            "\\n%{http_code} %header{x-ratelimit-reset} %header{retry-after}",
            &url,
        ];
        let argv = argv.map(str::to_owned).to_vec();
        let oversized = "Oversized GitHub response";
        let request = Request::fixed(argv, 5.0, MAX_RESPONSE + 512, oversized);
        let output = self.host.commands.run(&request).map_err(|_| Failure::Os)?;
        let split = output
            .iter()
            .rposition(|byte| *byte == b'\n')
            .ok_or(Failure::Os)?;
        let (body, trailer) = output.split_at(split);
        let trailer = String::from_utf8_lossy(&trailer[1..]).into_owned();
        let mut fields = trailer.split(' ');
        let status = fields.next().unwrap_or_default();
        if status != "200" {
            if ["403", "429"].contains(&status) {
                let decimal = |field: Option<&str>| {
                    let field = field.unwrap_or_default().trim();
                    let digits =
                        !field.is_empty() && field.bytes().all(|byte| byte.is_ascii_digit());
                    digits.then(|| field.parse::<f64>().ok()).flatten()
                };
                let mut wait_until = now + INTERVAL;
                if let Some(reset) = decimal(fields.next()) {
                    wait_until = wait_until.max(reset.min(now + 3600.0));
                }
                if let Some(retry) = decimal(fields.next()) {
                    wait_until = wait_until.max(now + retry.min(3600.0));
                }
                self.not_before = Some(wait_until);
            }
            return Err(Failure::Os);
        }
        require(body.len() <= MAX_RESPONSE, oversized)?;
        let value = state::decode(body)?;
        value.object().map(|_| value.clone()).ok_or(Failure::Value)
    }
}

/// One completed run of a fixed workflow, as far as the listing describes it.
#[derive(Clone, Debug, PartialEq)]
struct Run {
    run_id: i128,
    attempt: i128,
    revision: String,
    created_at: String,
    updated_at: String,
    workflow: usize,
}

/// Inspect at most 200 recent runs of one workflow, with explicit truncation.
fn runs(github: &mut Github<'_>, workflow: usize, since: &str) -> Result<(Vec<Run>, bool)> {
    let name = WORKFLOWS[workflow].name;
    let mut found: Vec<Run> = Vec::new();
    let mut raw = std::collections::BTreeSet::new();
    let mut total = 0;
    let mut first_total = None;
    for page in 1..=2_i128 {
        let created = since.replace(':', "%3A");
        let window = format!("branch=main&created=%3E%3D{created}");
        let query = format!("{window}&per_page=100&page={page}&exclude_pull_requests=true");
        let value = github.get(&format!("workflows/{name}.yml/runs?{query}"))?;
        total = whole(value.get("total_count"))?;
        first_total.get_or_insert(total);
        let entries = array(value.get("workflow_runs"))?;
        require(
            total >= 0 && entries.len() <= PAGE_SIZE,
            "Invalid workflow run count",
        )?;
        for run in entries {
            run.object().ok_or(Failure::Value)?;
            let repository = |field: &str| -> Result<bool> {
                let named = object(run.get(field))?
                    .get("full_name")
                    .and_then(Json::string);
                Ok(named == Some(github.deployment.repository.as_str()))
            };
            let path = format!(".github/workflows/{name}.yml");
            let ours = run.get("path").and_then(Json::string) == Some(path.as_str())
                && repository("repository")?
                && repository("head_repository")?;
            require(ours, "Workflow repository identity differs")?;
            let identity = whole(run.get("id"))?;
            raw.insert(identity);
            let field = |name: &str| run.get(name).and_then(Json::string);
            let scheduled =
                ["schedule", "workflow_dispatch"].contains(&field("event").unwrap_or_default());
            if field("head_branch") != Some("main") || !scheduled {
                continue;
            }
            let attempt = whole(run.get("run_attempt"))?;
            let revision = text(run.get("head_sha"))?;
            let identified = identity > 0
                && (1..=MAX_ATTEMPT_ID).contains(&attempt)
                && collect::revision(revision);
            require(identified, "Invalid workflow run identity")?;
            instant(run.get("created_at"))?;
            instant(run.get("updated_at"))?;
            if field("status") != Some("completed") {
                continue;
            }
            // The maintained workflow names its target in its run title; other
            // targets and unbound runs are unknown.
            let title = format!("SimplestChat {name}: {}", github.deployment.origin);
            if field("display_title") != Some(title.as_str()) {
                continue;
            }
            let run = Run {
                run_id: identity,
                attempt,
                revision: revision.to_owned(),
                created_at: text(run.get("created_at"))?.to_owned(),
                updated_at: text(run.get("updated_at"))?.to_owned(),
                workflow,
            };
            match found.iter_mut().find(|known| known.run_id == identity) {
                Some(known) => *known = run,
                None => found.push(run),
            }
        }
        if total <= page * 100 {
            break;
        }
    }
    let counted = i128::try_from(raw.len()).map_err(|_| Failure::Value)?;
    let truncated = total > MAX_RUNS || Some(total) != first_total || counted != total;
    Ok((found, truncated))
}

/// One attempt's verdict, from its actual job and steps.
#[derive(Clone, Debug, serde::Serialize)]
struct Classified {
    #[serde(rename = "runId")]
    run_id: i128,
    attempt: i128,
    #[serde(rename = "sourceRevision")]
    revision: String,
    #[serde(rename = "startedAt")]
    started_at: String,
    #[serde(rename = "completedAt")]
    completed_at: String,
    result: &'static str,
    conclusion: String,
    checks: Checks,
}

/// Each expected step's verdict, in the workflow's order.
#[derive(Clone, Debug)]
struct Checks(Vec<(&'static str, &'static str)>);

impl serde::Serialize for Checks {
    fn serialize<S: serde::Serializer>(
        &self,
        serializer: S,
    ) -> std::result::Result<S::Ok, S::Error> {
        serializer.collect_map(self.0.iter().copied())
    }
}

/// Validate one attempt's jobs and steps. An empty, skipped or setup-only run
/// cannot pass.
fn classify(github: &mut Github<'_>, run: &Run, attempt: i128) -> Result<Classified> {
    let workflow = &WORKFLOWS[run.workflow];
    let value = github.get(&format!(
        "runs/{}/attempts/{attempt}/jobs?per_page=100",
        run.run_id
    ))?;
    let jobs = array(value.get("jobs"))?;
    let complete = whole(value.get("total_count"))? == i128::try_from(jobs.len()).unwrap_or(-1)
        && jobs.len() <= MAX_JOBS;
    require(complete, "Incomplete external jobs response")?;
    for job in jobs {
        job.object().ok_or(Failure::Value)?;
    }
    let named = |job: &&Json| job.get("name").and_then(Json::string) == Some(workflow.job);
    let matches: Vec<&Json> = jobs.iter().filter(named).collect();
    let mut checks: Vec<(&str, &str)> = workflow
        .steps
        .iter()
        .map(|(key, _)| (*key, "missing"))
        .collect();
    let mut conclusion = "unknown".to_owned();
    let updated = instant(Some(&Json::String(run.updated_at.clone())))?;
    let (mut started, mut completed) = (updated, updated);
    if let ([job], 1) = (matches.as_slice(), jobs.len()) {
        let same = job
            .get("run_id")
            .is_some_and(|id| whole(Some(id)) == Ok(run.run_id))
            && job.get("head_sha").and_then(Json::string) == Some(run.revision.as_str());
        require(same, "External job identity differs")?;
        let numbered = job
            .get("run_attempt")
            .is_none_or(|value| whole(Some(value)) == Ok(attempt));
        require(numbered, "External job attempt differs")?;
        let finished = job.get("status").and_then(Json::string) == Some("completed");
        require(finished, "External job is unfinished")?;
        started = instant(Some(job.get("started_at").ok_or(Failure::Key)?))?;
        completed = instant(Some(job.get("completed_at").ok_or(Failure::Key)?))?;
        let latest = github.host.clock.now() + chrono::Duration::minutes(5);
        require(
            started <= completed && completed <= latest,
            "External job timestamps differ",
        )?;
        conclusion = match job.get("conclusion") {
            None => "unknown".to_owned(),
            Some(value) => value.string().ok_or(Failure::Value)?.to_owned(),
        };
        require(
            CONCLUSIONS.contains(&conclusion.as_str()),
            "Unknown external job conclusion",
        )?;
        let steps = array(job.get("steps"))?;
        for step in steps {
            step.object().ok_or(Failure::Value)?;
        }
        require(
            steps.len() <= MAX_STEPS,
            "Oversized external steps response",
        )?;
        for (at, (_, expected)) in workflow.steps.iter().enumerate() {
            let field =
                |step: &Json, name: &str| step.get(name).and_then(Json::string).map(str::to_owned);
            let selected: Vec<&Json> = steps
                .iter()
                .filter(|step| field(step, "name").as_deref() == Some(*expected))
                .collect();
            if let [step] = selected.as_slice()
                && field(step, "status").as_deref() == Some("completed")
            {
                match field(step, "conclusion").as_deref() {
                    Some("success") => checks[at].1 = "success",
                    Some("failure" | "timed_out") => checks[at].1 = "failure",
                    _ => {}
                }
            }
        }
    }
    let result = if conclusion == "success" && checks.iter().all(|(_, check)| *check == "success") {
        "success"
    } else if checks.iter().any(|(_, check)| *check == "failure") {
        "failure"
    } else {
        "incomplete"
    };
    Ok(Classified {
        run_id: run.run_id,
        attempt,
        revision: run.revision.clone(),
        started_at: iso(started),
        completed_at: iso(completed),
        result,
        conclusion,
        checks: Checks(checks),
    })
}

/// What one poll learned about one workflow.
#[derive(Debug, serde::Serialize)]
struct Batch {
    workflow: &'static str,
    #[serde(rename = "apiOk")]
    api_ok: bool,
    complete: bool,
    pending: u64,
    truncated: bool,
    #[serde(rename = "hasCompleted")]
    has_completed: bool,
    runs: Vec<Classified>,
}

/// One poll's snapshot, as the spool and the database statement take it.
#[derive(Debug, serde::Serialize)]
struct Snapshot<'a> {
    #[serde(rename = "observedAt")]
    observed_at: String,
    #[serde(rename = "windowStart")]
    window_start: String,
    revision: &'a str,
    deployment: &'a Deployment,
    workflows: Vec<Batch>,
}

/// Build a finite attempt queue, keeping every workflow's fetch status. One
/// current attempt of each workflow precedes historical backfill.
fn discover(
    github: &mut Github<'_>,
    seen: &BTreeMap<String, Seen>,
    since: &str,
) -> (Vec<Batch>, Vec<(Run, i128, String)>) {
    let mut batches = Vec::new();
    let mut pending = Vec::new();
    for (at, workflow) in WORKFLOWS.iter().enumerate() {
        let mut batch = Batch {
            workflow: workflow.name,
            api_ok: true,
            complete: false,
            pending: 0,
            truncated: false,
            has_completed: false,
            runs: Vec::new(),
        };
        match runs(github, at, since) {
            Ok((entries, truncated)) => {
                batch.has_completed = !entries.is_empty();
                batch.truncated = truncated;
                for run in entries {
                    if run.attempt > MAX_ATTEMPTS {
                        batch.truncated = true;
                    }
                    for attempt in (run.attempt - MAX_ATTEMPTS + 1).max(1)..=run.attempt {
                        let key = format!("{}:{}:{attempt}", workflow.name, run.run_id);
                        if !seen.contains_key(&key) {
                            pending.push((run.clone(), attempt, key));
                        }
                    }
                }
            }
            Err(_) => batch.api_ok = false,
        }
        batches.push(batch);
    }
    // Newest first, stably: a stable ascending sort of the reversed queue,
    // reversed again, is the stable descending order.
    pending.reverse();
    pending.sort_by(|a, b| (&a.0.created_at, a.1).cmp(&(&b.0.created_at, b.1)));
    pending.reverse();
    let mut ordered: Vec<(Run, i128, String)> = Vec::new();
    for at in 0..WORKFLOWS.len() {
        if let Some(item) = pending.iter().find(|item| item.0.workflow == at) {
            ordered.push(item.clone());
        }
    }
    for item in pending {
        if !ordered.iter().any(|known| known.2 == item.2) {
            ordered.push(item);
        }
    }
    (batches, ordered)
}

/// Prefer current results while keeping explicit pending historical work.
fn collect_snapshot<'a>(
    host: &'a Host<'a>,
    state: &mut State,
    revision: &'a str,
    deployment: &'a Deployment,
) -> Result<Snapshot<'a>> {
    let now = host.clock.now();
    let mut previous = match &state.observed_at {
        Some(observed) => alerts::instant(observed)?,
        None => now,
    };
    let spool = host.paths.state.join("external-spool");
    if let Some(newest) = alerts::entries(&spool)?.last() {
        let data = std::fs::read(newest)?;
        require(data.len() <= MAX_SNAPSHOT, "Oversized external spool entry")?;
        let latest = state::decode(&data)?;
        latest.object().ok_or(Failure::Value)?;
        previous = previous.max(alerts::instant(text(latest.get("observedAt"))?)?);
    }
    let observed = iso(now.max(previous + chrono::Duration::microseconds(1)));
    state.observed_at = Some(observed.clone());
    let since = (now - chrono::Duration::days(1))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    state
        .seen
        .retain(|_, seen| seen.created_at.as_str() >= since.as_str());
    let mut github = Github {
        host,
        deployment,
        requests: 0,
        not_before: state.not_before,
    };
    let (mut batches, pending) = discover(&mut github, &state.seen, &since);
    for (run, attempt, key) in pending {
        let batch = &mut batches[run.workflow];
        if github.requests >= MAX_REQUESTS {
            batch.pending += 1;
            continue;
        }
        match classify(&mut github, &run, attempt) {
            Ok(value) => {
                batch.runs.push(value);
                state.seen.insert(
                    key,
                    Seen {
                        created_at: run.created_at.clone(),
                    },
                );
            }
            Err(_) => {
                batch.api_ok = false;
                batch.pending += 1;
            }
        }
    }
    state.not_before = github.not_before;
    require(
        state.seen.len() <= MAX_SEEN,
        "External check cache exceeded its bound",
    )?;
    for batch in &mut batches {
        batch.complete =
            batch.api_ok && batch.has_completed && batch.pending == 0 && !batch.truncated;
    }
    Ok(Snapshot {
        observed_at: observed,
        window_start: since,
        revision,
        deployment,
        workflows: batches,
    })
}

/// Commit at most four idempotent snapshots per poll; a failed write keeps
/// its snapshot. Returns whether all offered were taken and how many remain.
fn replay(host: &Host<'_>) -> Result<(bool, usize)> {
    let spool = host.paths.state.join("external-spool");
    let statement = std::fs::read(host.paths.sql.join("monitoring-external.sql"))?;
    let mut healthy = true;
    for path in alerts::entries(&spool)?.iter().take(4) {
        let data = std::fs::read(path)?;
        require(data.len() <= MAX_SNAPSHOT, "Oversized external spool entry")?;
        let committed = String::from_utf8(data)
            .map_err(|_| Failure::Value)
            .and_then(|payload| database(host, &statement, Some(&payload)));
        if committed.is_err() {
            healthy = false;
            break;
        }
        std::fs::remove_file(path)?;
    }
    Ok((healthy, alerts::entries(&spool)?.len()))
}

/// Read committed check coverage; an unfinished import is never a recovery.
fn coverage(host: &Host<'_>) -> Result<Vec<String>> {
    let data = state::decode(&database(host, COVERAGE.as_bytes(), None)?)?;
    let mut samples = BTreeMap::new();
    for item in data.array().ok_or(Failure::Value)? {
        item.object().ok_or(Failure::Value)?;
        samples.insert(text(item.get("workflow"))?.to_owned(), item);
    }
    let mut lines = Vec::new();
    for workflow in &WORKFLOWS {
        let sample = samples.get(workflow.name);
        let latest = loose(sample.and_then(|sample| sample.get("latest")))?;
        require(latest >= 0.0, "Invalid external coverage timestamp")?;
        let flag = |name: &str| {
            u8::from(
                sample
                    .and_then(|sample| sample.get(name))
                    .and_then(Json::boolean)
                    == Some(true),
            )
        };
        let label = format!("{{component=\"{}\"}}", workflow.name);
        lines.push(format!(
            "simplestchat_ops_external_probe_timestamp_seconds{label} {}",
            float(latest)
        ));
        lines.push(format!(
            "simplestchat_ops_external_check_failed{label} {}",
            flag("failed")
        ));
        lines.push(format!(
            "simplestchat_ops_external_probe_complete{label} {}",
            flag("conclusive")
        ));
    }
    Ok(lines)
}

/// Read the importer's state, refusing shapes it never wrote.
fn load(host: &Host<'_>) -> Result<(State, bool)> {
    let value = state::object_or_empty(&host.paths.state.join("external.json"), 1024 * 1024)?;
    let empty = value
        .object()
        .is_none_or(std::collections::BTreeMap::is_empty);
    let mut seen = BTreeMap::new();
    if let Some(entries) = value.get("seen") {
        for (key, entry) in entries.object().ok_or(Failure::Value)? {
            let created_at = text(object(Some(entry))?.get("createdAt"))?.to_owned();
            seen.insert(key.clone(), Seen { created_at });
        }
    }
    let deployment = match value.get("deployment") {
        Some(found) => {
            let field = |name: &str| found.get(name).and_then(Json::string).unwrap_or_default();
            let two = found.object().is_some_and(|fields| fields.len() == 2);
            Some((
                two,
                field("repository").to_owned(),
                field("origin").to_owned(),
            ))
        }
        None => None,
    };
    let state = State {
        deployment: deployment
            .and_then(|(two, repository, origin)| two.then_some(Deployment { repository, origin })),
        last_poll_at: loose(value.get("lastPollAt"))?,
        observed_at: match value.get("observedAt") {
            Some(observed) => Some(observed.string().ok_or(Failure::Value)?.to_owned()),
            None => None,
        },
        seen,
        not_before: match value.get("notBefore") {
            Some(_) => Some(loose(value.get("notBefore"))?),
            None => None,
        },
        dropped: match value.get("dropped") {
            Some(_) => whole(value.get("dropped"))?,
            None => 0,
        },
    };
    Ok((state, empty))
}

/// Poll once under the importer's lock, at most every ten minutes. Returns
/// the line for the journal, or nothing when the interval has not passed.
pub(super) fn import(host: &Host<'_>) -> Result<Option<&'static str>> {
    let directory = &host.paths.state;
    let lock = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(directory.join("external.lock"))?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive)
        .map_err(|_| Failure::Os)?;
    let path = directory.join("external.json");
    let (mut state, empty) = load(host)?;
    let now = seconds(host.clock.now());
    if now < state.last_poll_at + INTERVAL {
        return Ok(None);
    }
    let deployment = configured(host)?;
    // Changing provenance, or adopting a history without one, needs a
    // reviewed migration; it is never silently reassigned.
    let bound = empty || state.deployment.as_ref() == Some(&deployment);
    let foreign =
        "External evidence is unattributed or belongs to another deployment; review migration";
    require(bound, foreign)?;
    state.deployment = Some(deployment.clone());
    state.last_poll_at = now;
    state::atomic_json(&path, &state)?;
    let (_, revision) = collect::setting(&host.paths.images, "revision")?;
    require(collect::revision(&revision), "Invalid deployed revision")?;
    let value = collect_snapshot(host, &mut state, &revision, &deployment)?;
    let data = serde_json::to_vec(&value).map_err(|_| Failure::Value)?;
    let spool = directory.join("external-spool");
    let bound = alerts::Bound {
        entries: MAX_SPOOL,
        bytes: MAX_SNAPSHOT,
        oversized: "External snapshot exceeded its bound",
    };
    let (_, discarded) = alerts::persist(&spool, &data, host.clock.now(), &bound)?;
    state.dropped += i128::try_from(discarded).map_err(|_| Failure::Value)?;
    state::atomic_json(&path, &state)?;
    let (mut healthy, pending) = replay(host)?;
    let committed = match coverage(host) {
        Ok(lines) => lines,
        Err(Failure::Key) => return Err(Failure::Key),
        Err(_) => {
            healthy = false;
            Vec::new()
        }
    };
    let mut lines = vec![
        format!(
            "simplestchat_ops_external_import_timestamp_seconds {}",
            float(now)
        ),
        format!(
            "simplestchat_ops_external_database_ok {}",
            u8::from(healthy)
        ),
        format!("simplestchat_ops_external_spool_snapshots {pending}"),
        format!(
            "simplestchat_ops_external_spool_dropped_total {}",
            state.dropped
        ),
    ];
    lines.extend(committed);
    for batch in &value.workflows {
        let label = format!("{{component=\"{}\"}}", batch.workflow);
        lines.push(format!(
            "simplestchat_ops_external_api_ok{label} {}",
            u8::from(batch.api_ok)
        ));
        let complete = u8::from(batch.complete);
        lines.push(format!(
            "simplestchat_ops_external_coverage_complete{label} {complete}"
        ));
        lines.push(format!(
            "simplestchat_ops_external_pending{label} {}",
            batch.pending
        ));
    }
    let target = directory.join("textfile/external.prom");
    state::atomic(&target, (lines.join("\n") + "\n").as_bytes())?;
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644))?;
    Ok(Some(
        "External check import completed; review private coverage report.\n",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::command::tests::Scripted;
    use crate::host::state::tests::Scratch;
    use crate::host::tests::{Fixed, paths};
    use serde_json::Value;
    use std::os::unix::fs::MetadataExt;

    /// Answer one request as the declared world of a recorded poll did: the
    /// API through the transfer tool's output shape, the database by mode.
    fn transport(request: &Request<'_>, poll: &Value) -> Result<Vec<u8>> {
        let last = request.argv.last().unwrap();
        if request.argv[0] == "/usr/bin/curl" {
            let Some(answer) = poll["world"].get(last) else {
                return Err(Failure::Command);
            };
            let header = |name: &str| {
                answer["headers"][name]
                    .as_str()
                    .unwrap_or_default()
                    .to_owned()
            };
            let body = match answer["raw"].as_str() {
                Some(raw) => raw.to_owned(),
                None if answer["status"] == 200 => answer["body"].to_string(),
                None => String::new(),
            };
            let reset = header("X-RateLimit-Reset");
            let retry = header("Retry-After");
            return Ok(format!("{body}\n{} {reset} {retry}", answer["status"]).into_bytes());
        }
        let mode = poll["database"].as_str().unwrap();
        if last.starts_with("payload=") {
            return if mode == "replay-fails" {
                Err(Failure::Command)
            } else {
                Ok(Vec::new())
            };
        }
        if mode == "coverage-fails" {
            return Err(Failure::Command);
        }
        Ok(poll["coverage"].to_string().into_bytes())
    }

    /// Every number as a float: a whole number kept from an earlier state is
    /// the same value written with a fraction.
    fn numbers(value: Value) -> Value {
        match value {
            Value::Number(number) => serde_json::json!(number.as_f64().unwrap()),
            Value::Array(items) => Value::Array(items.into_iter().map(numbers).collect()),
            Value::Object(fields) => Value::Object(
                fields
                    .into_iter()
                    .map(|(key, item)| (key, numbers(item)))
                    .collect(),
            ),
            other => other,
        }
    }

    fn words(text: &str) -> Vec<&str> {
        text.split_whitespace().collect()
    }

    /// Fourteen recorded worlds, twenty-three polls: every poll makes the
    /// recorded requests in order, offers the database the recorded snapshots
    /// byte for byte, and leaves the recorded textfile, spool and state.
    #[test]
    fn every_recorded_poll_is_reproduced_request_by_request() {
        let recorded: Value = serde_json::from_str(include_str!("recorded_external.json")).unwrap();
        let base = DateTime::parse_from_rfc3339(recorded["base"].as_str().unwrap()).unwrap();
        let scenarios = recorded["scenarios"].as_array().unwrap();
        assert_eq!(scenarios.len(), 19);
        let mut polled = 0;
        for scenario in scenarios {
            let name = scenario["name"].as_str().unwrap();
            let scratch = Scratch::new();
            let place = paths(scratch.path());
            std::fs::create_dir_all(place.state.join("textfile")).unwrap();
            std::fs::create_dir_all(&place.sql).unwrap();
            std::fs::write(place.sql.join("monitoring-external.sql"), "<external>").unwrap();
            std::fs::write(&place.settings, scenario["settings"].to_string()).unwrap();
            let private = std::fs::Permissions::from_mode(0o600);
            std::fs::set_permissions(&place.settings, private).unwrap();
            let images = serde_json::json!({"revision": recorded["revision"]});
            std::fs::write(&place.images, images.to_string()).unwrap();
            if !scenario["initialState"].is_null() {
                let text = scenario["initialState"].to_string();
                std::fs::write(place.state.join("external.json"), text).unwrap();
            }
            let polls = scenario["polls"].as_array().unwrap();
            if let Some(entries) = scenario["initialSpool"].as_object() {
                std::fs::create_dir(place.state.join("external-spool")).unwrap();
                for entry in entries.keys() {
                    // The exact earlier bytes, as the first poll still held them.
                    let text = polls[0]["spool"][entry].as_str().unwrap();
                    std::fs::write(place.state.join("external-spool").join(entry), text).unwrap();
                }
            }
            let owner = std::fs::metadata(scratch.path()).unwrap().uid();
            for poll in polls {
                polled += 1;
                let offset = poll["offset"].as_i64().unwrap();
                let clock = Fixed::at("2026-01-01T00:00:00Z");
                clock
                    .now
                    .set((base + chrono::Duration::seconds(offset)).with_timezone(&Utc));
                let commands = Scripted::new(|request| transport(request, poll));
                let host = Host {
                    commands: &commands,
                    clock: &clock,
                    paths: paths(scratch.path()),
                    owner,
                };
                let outcome = import(&host);
                let label = format!("{name} at +{offset}");
                match &outcome {
                    Ok(line) => {
                        assert_eq!(poll["status"], 0, "{label}");
                        assert_eq!(
                            line.unwrap_or_default(),
                            poll["stdout"].as_str().unwrap(),
                            "{label}"
                        );
                    }
                    Err(failure) => {
                        assert_eq!(poll["status"], 1, "{label}: {failure:?}");
                        if let Failure::Refused(message) = failure {
                            assert_eq!(Some(*message), poll["failure"].as_str(), "{label}");
                        }
                    }
                }
                let asked = commands.asked.borrow();
                let fetched = asked
                    .iter()
                    .filter(|asked| asked.argv[0] == "/usr/bin/curl");
                let fetched: Vec<&str> = fetched
                    .map(|asked| asked.argv.last().unwrap().as_str())
                    .collect();
                let requests = poll["requests"].as_array().unwrap();
                let requests: Vec<&str> = requests
                    .iter()
                    .map(|request| request["url"].as_str().unwrap())
                    .collect();
                assert_eq!(fetched, requests, "{label}");
                let statements: Vec<_> = asked
                    .iter()
                    .filter(|asked| asked.argv[0] != "/usr/bin/curl")
                    .collect();
                let expected = poll["statements"].as_array().unwrap();
                assert_eq!(statements.len(), expected.len(), "{label}");
                for (ours, theirs) in statements.iter().zip(expected) {
                    let input = String::from_utf8(ours.input.clone().unwrap()).unwrap();
                    if theirs["kind"] == "external" {
                        assert_eq!(input, "<external>", "{label}");
                        let payload = ours.argv.last().unwrap().strip_prefix("payload=");
                        assert_eq!(payload, theirs["payload"].as_str(), "{label}");
                    } else {
                        assert_eq!(
                            words(&input),
                            words(theirs["statement"].as_str().unwrap()),
                            "{label}"
                        );
                    }
                }
                drop(asked);
                let prom =
                    std::fs::read_to_string(host.paths.state.join("textfile/external.prom")).ok();
                assert_eq!(prom.as_deref(), poll["prom"].as_str(), "{label}");
                let mut spool = serde_json::Map::new();
                for path in alerts::entries(&host.paths.state.join("external-spool")).unwrap() {
                    let entry = path.file_name().unwrap().to_str().unwrap().to_owned();
                    spool.insert(
                        entry,
                        Value::String(std::fs::read_to_string(&path).unwrap()),
                    );
                }
                assert_eq!(Value::Object(spool), poll["spool"], "{label}");
                let state = std::fs::read(host.paths.state.join("external.json")).ok();
                let state: Value =
                    state.map_or(Value::Null, |bytes| serde_json::from_slice(&bytes).unwrap());
                assert_eq!(numbers(state), numbers(poll["state"].clone()), "{label}");
            }
        }
        assert_eq!(polled, 28);
    }

    /// From `test_fixed_origin_budget_response_limit_and_rate_backoff`: a
    /// request is one fixed command to the fixed API host for the configured
    /// repository, with no credential, proxy, configuration file or
    /// redirect, bounded in time and size. Any other path issues nothing and
    /// spends nothing of the budget.
    #[test]
    fn requests_are_fixed_and_unauthenticated_and_other_paths_issue_none() {
        let scratch = Scratch::new();
        let commands = Scripted::new(|_: &Request<'_>| Ok(b"{}\n200  ".to_vec()));
        let clock = Fixed::at("2026-11-11T12:00:00Z");
        let host = Host {
            commands: &commands,
            clock: &clock,
            paths: paths(scratch.path()),
            owner: 0,
        };
        let deployment = Deployment::new("example/chat", "https://chat.example.test").unwrap();
        let mut github = Github {
            host: &host,
            deployment: &deployment,
            requests: 0,
            not_before: None,
        };
        for path in [
            "",
            "https://elsewhere.example/runs",
            "../../users/example",
            "runs/1/attempts/1/logs",
            "runs/01/attempts/1/jobs?per_page=100",
            "runs/1/attempts/1/jobs?per_page=100&page=2",
            "workflows/release.yml/runs?branch=main",
            "workflows/canary.yml/runs?",
            "workflows/canary.yml/runs?branch=main#fragment",
            "workflows/canary.yml/runs?branch=main&created=>2026",
        ] {
            let refused = github.get(path).map(|_| ());
            assert_eq!(
                refused,
                Err(Failure::Refused("Unexpected GitHub API path")),
                "{path}"
            );
        }
        assert!(commands.asked.borrow().is_empty());
        assert_eq!(github.requests, 0);
        github.get("runs/7/attempts/2/jobs?per_page=100").unwrap();
        let asked = commands.asked.borrow();
        assert_eq!(asked.len(), 1);
        assert_eq!(
            asked[0].argv,
            [
                "/usr/bin/curl",
                "--disable",
                "--silent",
                "--show-error",
                "--noproxy",
                "*",
                "--max-redirs",
                "0",
                "--proto",
                "=https",
                "--max-time",
                "3",
                "--header",
                "Accept: application/vnd.github+json",
                "--header",
                "X-GitHub-Api-Version: 2026-03-10",
                "--header",
                "User-Agent: simplestchat-private-monitor/1",
                "--output",
                "-",
                "--write-out",
                "\\n%{http_code} %header{x-ratelimit-reset} %header{retry-after}",
                "https://api.github.com/repos/example/chat/actions/runs/7/attempts/2/jobs?per_page=100",
            ]
        );
        let fixed = [("PATH", "/usr/bin:/bin"), ("HOME", "/root")];
        assert_eq!(
            asked[0].environment,
            fixed.map(|(key, value)| (key.to_owned(), value.to_owned()))
        );
        assert!(
            asked[0].input.is_none() && asked[0].source.is_none() && asked[0].archive.is_none()
        );
        assert!((asked[0].seconds - 5.0).abs() < f64::EPSILON);
        assert_eq!(asked[0].limit, 4 * 1024 * 1024 + 512);
        assert_eq!(MAX_REQUESTS, 8);
    }

    /// From `test_expected_workflow_jobs_and_steps_match_repository`: a
    /// workflow rename must change this contract in the same commit. Each
    /// workflow titles its runs with the configured origin, has one job with
    /// the expected name, and names each checked step exactly once.
    #[test]
    fn expected_workflow_jobs_and_steps_match_the_repository() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap();
        for workflow in &WORKFLOWS {
            let name = workflow.name;
            let path = root.join(format!(".github/workflows/{name}.yml"));
            let text = std::fs::read_to_string(path).unwrap();
            let lines: Vec<&str> = text.lines().collect();
            let count = |wanted: &str| lines.iter().filter(|line| **line == wanted).count();
            let title = format!("run-name: \"SimplestChat {name}: ${{{{ vars.CANARY_ORIGIN }}}}\"");
            assert_eq!(count(&title), 1, "{name}");
            let jobs = lines.iter().position(|line| *line == "jobs:").unwrap();
            let key = |line: &&&str| {
                let indented = line
                    .strip_prefix("  ")
                    .is_some_and(|rest| !rest.starts_with([' ', '#']));
                indented && line.ends_with(':')
            };
            assert_eq!(lines[jobs + 1..].iter().filter(key).count(), 1, "{name}");
            let named = |line: &&&str| line.starts_with("    name: ");
            let names: Vec<&&str> = lines[jobs + 1..].iter().filter(named).collect();
            assert_eq!(
                names,
                [&format!("    name: {}", workflow.job).as_str()],
                "{name}"
            );
            for (_, step) in workflow.steps {
                assert_eq!(count(&format!("      - name: {step}")), 1, "{step}");
            }
        }
    }
}
