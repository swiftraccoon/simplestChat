//! The GitHub client: `gh api` on GitHub.com only, no shell, no retry,
//! every answer and page count bounded, the whole operation held to its
//! deadline, writes paced apart without ever waiting past it, and the
//! reference endpoint `git/ref/` only ever read. Time and the program it
//! runs are what the caller gives it, so that a test runs no real clock.

use super::{
    MAX_APPLY_SECONDS, MAX_PAGE, MAX_PAGES, MAX_SECONDS, MAX_SOURCE, PROCESS, WRITE_INTERVAL,
    array, classified, decode, expected, field, object, same, string,
};
use crate::{
    Fault, Result, commit,
    context::{self, Failure},
    document::Document,
    escaped, require,
};
use std::{
    collections::BTreeSet,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

/// The most bytes a program may complain in.
const COMPLAINT: u64 = 65536;
/// The longest one request may take, in seconds.
const REQUEST_SECONDS: f64 = 20.0;
/// The variable that would make `gh` print what it sends.
const DEBUG: &str = "GH_DEBUG";

/// The time of an operation, as the reference's monotonic clock measured
/// it, and its waiting.
pub trait Clock {
    /// Seconds since a fixed moment.
    fn now(&self) -> f64;
    /// Wait `seconds`.
    fn sleep(&self, seconds: f64);
}

/// How a program is run: to its end within its time, with what it reads,
/// in exactly the environment it is given.
pub trait Transport {
    /// The exit status of `program` run with `arguments` in the environment
    /// `variables`, having read `input`, and what it printed: it may take
    /// `within` seconds and print and complain in `room`.
    fn exchange(
        &self,
        program: &Path,
        arguments: &[String],
        variables: &[(OsString, OsString)],
        input: &[u8],
        within: f64,
        room: (u64, u64),
    ) -> Result<(i32, Vec<u8>)>;
}

/// The host's own clock, counted from when it was made, and its own
/// waiting.
#[derive(Debug)]
pub struct System(Instant);

impl System {
    /// The clock, starting now.
    pub fn new() -> Self {
        Self(Instant::now())
    }
}

impl Default for System {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for System {
    fn now(&self) -> f64 {
        self.0.elapsed().as_secs_f64()
    }

    fn sleep(&self, seconds: f64) {
        std::thread::sleep(Duration::try_from_secs_f64(seconds).unwrap_or_default());
    }
}

/// The class of what the reference's bounded runner raised where a
/// command did not end with a status.
fn unanswered(failure: Failure) -> Fault {
    match failure {
        Failure::Fixed(code) => classified(Fault(code)),
        Failure::Command(_) | Failure::Prerequisite(_) => PROCESS,
    }
}

/// Programs run as the reference's bounded runner ran them: in their own
/// process group, with both streams bounded, stopped at their deadline.
#[derive(Clone, Copy, Debug, Default)]
pub struct Bounded;

impl Transport for Bounded {
    fn exchange(
        &self,
        program: &Path,
        arguments: &[String],
        variables: &[(OsString, OsString)],
        input: &[u8],
        within: f64,
        room: (u64, u64),
    ) -> Result<(i32, Vec<u8>)> {
        let mut command = Command::new(program);
        command.args(arguments).env_clear();
        command.envs(variables.iter().map(|(name, value)| (name, value)));
        let within = Duration::try_from_secs_f64(within).map_err(|_| PROCESS)?;
        context::asked(&mut command, input, within, room).map_err(unanswered)
    }
}

/// What a client runs and keeps time with, and where it finds `gh`.
#[derive(Clone, Copy)]
pub struct Reach<'a> {
    /// Its clock.
    pub clock: &'a dyn Clock,
    /// How it runs `gh`.
    pub transport: &'a dyn Transport,
    /// The caller's environment, every variable of it.
    pub environment: &'a [(OsString, OsString)],
}

impl Reach<'_> {
    /// The caller's search path.
    pub fn search(&self) -> Option<&OsStr> {
        let found = self.environment.iter().find(|(name, _)| name == "PATH");
        found.map(|(_, value)| value.as_os_str())
    }
}

/// A client of one repository on GitHub.com, its operation bounded by one
/// deadline.
pub struct Github<'a> {
    /// The repository, `owner/name`.
    pub repository: String,
    /// When the operation's time is up.
    pub deadline: f64,
    /// The earliest moment of the next write.
    pub next_write: f64,
    /// The `gh` that is run.
    pub executable: PathBuf,
    /// The stored analysis categories a revision needs: the automated
    /// workflow's, unless a replay of an earlier recording names its own.
    pub(crate) categories: Vec<String>,
    clock: &'a dyn Clock,
    transport: &'a dyn Transport,
    variables: Vec<(OsString, OsString)>,
}

/// Whether `repository` is an explicit `owner/name`.
fn repository_named(repository: &str) -> bool {
    let plain = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
    };
    repository
        .split_once('/')
        .is_some_and(|(owner, name)| plain(owner) && plain(name))
}

/// Whether `reference` is a branch or a pull request's merge, each of its
/// steps a name.
fn reference_named(reference: &str) -> bool {
    let plain = |byte: u8| byte.is_ascii_alphanumeric() || b"._/-".contains(&byte);
    // A branch or pull request number of nothing leaves an empty step,
    // which no reference may have.
    let branch = reference
        .strip_prefix("refs/heads/")
        .is_some_and(|name| name.bytes().all(plain));
    let merge = reference
        .strip_prefix("refs/pull/")
        .and_then(|rest| rest.strip_suffix("/merge"))
        .is_some_and(|number| number.bytes().all(|byte| byte.is_ascii_digit()));
    let stepped = reference
        .split('/')
        .all(|step| !matches!(step, "" | "." | ".."));
    (branch || merge) && stepped
}

/// `pairs` as the query of a URL.
fn query(pairs: &[(&str, &str)]) -> String {
    let encoded: Vec<String> = pairs
        .iter()
        .map(|(name, value)| format!("{}={}", escaped(name, true), escaped(value, true)))
        .collect();
    encoded.join("&")
}

impl<'a> Github<'a> {
    /// A client of the explicit `repository`, whose operation may take as
    /// long as one that writes when `apply` says so, running the `gh` of
    /// the caller's search path.
    pub fn new(repository: &str, apply: bool, reach: Reach<'a>) -> Result<Self> {
        require(repository_named(repository), "codeql_repository")?;
        let allowed = if apply {
            MAX_APPLY_SECONDS
        } else {
            MAX_SECONDS
        };
        let deadline = reach.clock.now() + allowed;
        let executable = context::executable("gh", reach.search());
        let executable = executable.map_err(|_| Fault("codeql_gh_missing"))?;
        let mut variables: Vec<(OsString, OsString)> = reach
            .environment
            .iter()
            .filter(|(name, _)| name != DEBUG)
            .cloned()
            .collect();
        for (name, value) in [
            ("GH_PROMPT_DISABLED", "1"),
            ("GH_HOST", "github.com"),
            ("GH_PAGER", "cat"),
        ] {
            variables.retain(|(held, _)| held != name);
            variables.push((name.into(), value.into()));
        }
        Ok(Self {
            repository: repository.to_owned(),
            deadline,
            next_write: 0.0,
            executable,
            categories: expected(),
            clock: reach.clock,
            transport: reach.transport,
            variables,
        })
    }

    /// The client's clock.
    pub fn clock(&self) -> &'a dyn Clock {
        self.clock
    }

    /// Wait until the next write may be made, refreshing nothing; never
    /// wait past the deadline.
    pub fn prepare_write(&self) -> Result<()> {
        loop {
            let now = self.clock.now();
            let delay = f64::max(0.0, self.next_write - now);
            require(now + delay < self.deadline, "codeql_api_deadline")?;
            if delay == 0.0 {
                return Ok(());
            }
            self.clock.sleep(delay);
        }
    }

    /// What GitHub answers at `suffix` of the repository's code scanning,
    /// or of its references for a `git/ref/` one: read, or changed by
    /// `body` once the write's pacing allows.
    pub fn request(&mut self, suffix: &str, body: Option<&Document>) -> Result<Document> {
        if body.is_some() {
            self.prepare_write()?;
        }
        let remaining = self.deadline - self.clock.now();
        require(remaining > 0.0, "codeql_api_deadline")?;
        let reference_read = suffix.starts_with("git/ref/");
        require(!reference_read || body.is_none(), "codeql_ref_read_only")?;
        let endpoint = if reference_read {
            format!("repos/{}/{suffix}", self.repository)
        } else {
            format!("repos/{}/code-scanning/{suffix}", self.repository)
        };
        let method = if body.is_some() { "PATCH" } else { "GET" };
        let mut arguments: Vec<String> = [
            "api",
            "--hostname",
            "github.com",
            "--method",
            method,
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "X-GitHub-Api-Version: 2022-11-28",
        ]
        .map(str::to_owned)
        .to_vec();
        arguments.push(endpoint);
        if body.is_some() {
            arguments.extend(["--input".to_owned(), "-".to_owned()]);
        }
        let input = body.map(Document::spaced).unwrap_or_default();
        let within = f64::min(REQUEST_SECONDS, remaining);
        let exchanged = self.transport.exchange(
            &self.executable,
            &arguments,
            &self.variables,
            input.as_bytes(),
            within,
            (MAX_SOURCE, COMPLAINT),
        );
        if body.is_some() {
            self.next_write = self.clock.now() + WRITE_INTERVAL;
        }
        let (status, output) = exchanged?;
        require(status == 0, "codeql_api_failed")?;
        let result = decode(&output)?;
        require(self.clock.now() < self.deadline, "codeql_api_deadline")?;
        Ok(result)
    }

    /// The commit the exact branch or pull request merge `reference` is at
    /// now, as GitHub answers it.
    pub fn head(&mut self, reference: &str) -> Result<String> {
        require(reference_named(reference), "codeql_ref")?;
        let name = reference.strip_prefix("refs/").unwrap_or(reference);
        let raw = self.request(&format!("git/ref/{name}"), None)?;
        let raw = object(&raw)?;
        let target = object(field(raw, "object")?)?;
        require(
            same(field(raw, "ref")?, reference) && same(field(target, "type")?, "commit"),
            "codeql_ref_identity",
        )?;
        let revision = string(field(target, "sha")?)?;
        require(commit(revision), "codeql_ref_revision")?;
        Ok(revision.to_owned())
    }

    /// Every entry of `resource` at `reference`, newest first, of every
    /// state: a dismissal on GitHub is evidence, never authority. For the
    /// analyses of a `revision`, only the newest of each automated
    /// category is kept, and the pages end once each has been seen.
    pub fn pages(
        &mut self,
        resource: &str,
        reference: &str,
        revision: Option<&str>,
    ) -> Result<Vec<Document>> {
        let mut entries = Vec::new();
        let mut categories = BTreeSet::new();
        let expected = self.categories.clone();
        require(
            revision.is_none() || resource == "analyses",
            "codeql_page_revision",
        )?;
        for page in 1..=MAX_PAGES {
            let size = MAX_PAGE.to_string();
            let number = page.to_string();
            let asked = query(&[
                ("ref", reference),
                ("tool_name", "CodeQL"),
                ("per_page", &size),
                ("page", &number),
                ("sort", "created"),
                ("direction", "desc"),
            ]);
            let answer = self.request(&format!("{resource}?{asked}"), None)?;
            let selected = array(&answer)?;
            require(selected.len() <= MAX_PAGE, "codeql_page_size")?;
            for item in selected {
                let value = object(item)?;
                if revision.is_some() {
                    let category = string(field(value, "category")?)?.trim_end_matches('/');
                    if !expected.iter().any(|held| held == category)
                        || categories.contains(category)
                    {
                        continue;
                    }
                    categories.insert(category.to_owned());
                }
                entries.push(value.clone());
            }
            if revision.is_some() && expected.iter().all(|held| categories.contains(held)) {
                return Ok(entries);
            }
            if selected.len() < MAX_PAGE {
                return Ok(entries);
            }
        }
        Err(Fault("codeql_pagination_limit"))
    }
}

#[cfg(test)]
#[path = "triage_github_tests.rs"]
pub(crate) mod tests;
