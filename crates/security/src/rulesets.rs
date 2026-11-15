//! The reconciliation of the repository's rulesets with their policy.
//!
//! `security/rulesets.json` names the one ruleset that protects the history
//! of `main`: nobody deletes the branch and nobody rewrites it, and nobody
//! bypasses that. `plan` and `check` compare what GitHub holds with the
//! policy and change nothing. `apply` creates or updates that one ruleset,
//! and only from a clean checkout of the very commit `main` is at, whose
//! gate, trusted run and analyses all passed. No other ruleset is touched
//! and none is ever deleted or disabled. The head of `main` is asked again
//! before the write and on both sides of the reading back.
//!
//! GitHub is asked through `gh`, which holds the caller's credentials, so
//! the two programs run in the caller's own environment. Nothing they
//! complain of is repeated: a failure is one fixed code.

use crate::{
    Fault, Result, commit,
    context::{self, Outcome},
    files,
    record::{self, quoted},
    require,
};
use serde_json::{Map, Value, json};
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    process::{Command, ExitCode},
    time::Duration,
};

/// Where the policy is, below the checkout.
const POLICY: &str = "security/rulesets.json";
/// The most bytes of the policy.
const MAX_POLICY: u64 = 65536;
/// What of a ruleset the policy says and GitHub is held to, in the order
/// of the names.
const FIELDS: [&str; 6] = [
    "bypass_actors",
    "conditions",
    "enforcement",
    "name",
    "rules",
    "target",
];
/// The one ruleset that is managed.
const NAME: &str = "Main history protection";
/// Rulesets this repository once had and may not keep, in the order of
/// their names.
const OBSOLETE: [&str; 2] = ["Main pull request review", "Main security gates"];
/// The check that every other required check answers to.
const GATE: &str = "Required security and correctness checks";
/// The application that reports the checks of GitHub Actions.
const GITHUB_ACTIONS: u64 = 15368;
/// The languages whose analysis is required.
const LANGUAGES: [&str; 4] = ["actions", "javascript-typescript", "python", "rust"];
/// How many records one page holds, and how many pages are read.
const PAGE: usize = 100;
/// How long one command has.
const COMMAND_TIME: Duration = Duration::from_secs(60);
/// The most bytes one command may print, and may complain.
const ROOM: (u64, u64) = (8 * 1024 * 1024, 65536);
/// A policy that is no JSON, or holds a value of another kind.
const POLICY_FORMAT: Fault = Fault("ruleset_policy_format");
/// An answer that is no JSON, or holds a value of another kind.
const ANSWER_FORMAT: Fault = Fault("ruleset_answer_format");

/// What the command is asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Say what would change.
    Plan,
    /// Say what would change, and fail when anything would.
    Check,
    /// Create or update the ruleset.
    Apply,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Plan => "plan",
            Self::Check => "check",
            Self::Apply => "apply",
        }
    }
}

/// The policy: the repository it is for, by name and by number, and the
/// ruleset it says the repository has.
#[derive(Debug, PartialEq)]
struct Policy {
    repository: String,
    identifier: u64,
    ruleset: Map<String, Value>,
}

/// Read the policy. It names one repository and exactly the managed
/// ruleset: active, for the branch `main` alone, with no bypass, and with
/// the two rules of history protection and no other.
fn policy(content: &[u8]) -> Result<Policy> {
    let document = record::strict(content).map_err(|_| POLICY_FORMAT)?;
    let fields = document.as_object().ok_or(POLICY_FORMAT)?;
    let keys: Vec<&str> = fields.keys().map(String::as_str).collect();
    require(
        keys == ["repository", "repositoryId", "rulesets"],
        "ruleset_policy_schema",
    )?;
    let repository = fields["repository"].as_str().ok_or(POLICY_FORMAT)?;
    let named = |byte: u8| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'.' | b'-');
    let part = |part: &str| !part.is_empty() && part.bytes().all(named);
    let parts = repository.split_once('/');
    let identifier = fields["repositoryId"].as_u64().filter(|number| *number > 0);
    let identified = parts.is_some_and(|(owner, name)| part(owner) && part(name));
    let Some(identifier) = identifier.filter(|_| identified) else {
        return Err(Fault("ruleset_repository_identity"));
    };
    let entries = fields["rulesets"].as_array().ok_or(POLICY_FORMAT)?;
    let entries: Vec<&Map<String, Value>> = entries
        .iter()
        .map(|entry| entry.as_object().ok_or(POLICY_FORMAT))
        .collect::<Result<_>>()?;
    let [entry] = entries.as_slice() else {
        return Err(Fault("ruleset_names"));
    };
    let name = entry.get("name").and_then(Value::as_str);
    require(name.ok_or(POLICY_FORMAT)? == NAME, "ruleset_names")?;
    let keys: Vec<&str> = entry.keys().map(String::as_str).collect();
    require(keys == FIELDS, "ruleset_fields")?;
    let main = json!({"ref_name": {"include": ["refs/heads/main"], "exclude": []}});
    require(
        entry["target"] == "branch"
            && entry["enforcement"] == "active"
            && entry["conditions"] == main,
        "ruleset_target",
    )?;
    require(
        entry["bypass_actors"] == json!([]),
        "history_bypass_forbidden",
    )?;
    let protected = json!([{"type": "deletion"}, {"type": "non_fast_forward"}]);
    require(
        ordered(&entry["rules"]).ok_or(POLICY_FORMAT)? == protected,
        "history_rules_differ",
    )?;
    Ok(Policy {
        repository: repository.to_owned(),
        identifier,
        ruleset: (*entry).clone(),
    })
}

/// The policy of the checkout at `root`.
fn loaded(root: &Path) -> Result<Policy> {
    policy(&files::bounded(&root.join(POLICY), MAX_POLICY, false)?)
}

/// The rules of a ruleset in one order, whatever order they were given in.
fn ordered(rules: &Value) -> Option<Value> {
    let mut rules = rules.as_array()?.clone();
    rules.sort_by_cached_key(Value::to_string);
    Some(Value::Array(rules))
}

/// What of a ruleset is compared: the fields the policy says, and the rules
/// in one order. What else GitHub answers about a ruleset is not compared.
fn comparable(ruleset: &Map<String, Value>) -> Result<Vec<Value>> {
    let compared = |field: &&str| {
        let value = ruleset.get(*field).ok_or(ANSWER_FORMAT)?;
        match *field {
            "rules" => ordered(value).ok_or(ANSWER_FORMAT),
            _ => Ok(value.clone()),
        }
    };
    FIELDS.iter().map(compared).collect()
}

/// Whether `value` is the text `text`.
fn is(value: Option<&Value>, text: &str) -> bool {
    value.and_then(Value::as_str) == Some(text)
}

/// The field `name` of an answer's record.
fn field<'a>(record: &'a Map<String, Value>, name: &str) -> Result<&'a Value> {
    record.get(name).ok_or(ANSWER_FORMAT)
}

/// An answer, or a part of one, as a record.
fn object(value: &Value) -> Result<&Map<String, Value>> {
    value.as_object().ok_or(ANSWER_FORMAT)
}

/// An answer, or a part of one, as a list of records.
fn records(value: &Value) -> Result<Vec<&Map<String, Value>>> {
    let listed = value.as_array().ok_or(ANSWER_FORMAT)?;
    listed.iter().map(object).collect()
}

/// The gate passed for `revision`: one check of that name among all the
/// checks there are, completed with success, for that commit, and reported
/// by GitHub Actions itself.
fn healthy_checks(answer: &Value, revision: &str) -> Result<()> {
    let answer = object(answer)?;
    let checks = records(field(answer, "check_runs")?)?;
    let counted = field(answer, "total_count")?.as_u64();
    require(
        counted == u64::try_from(checks.len()).ok(),
        "ruleset_check_pagination",
    )?;
    let mut gates = Vec::new();
    for check in checks {
        if is(Some(field(check, "name")?), GATE) {
            gates.push(check);
        }
    }
    let [gate] = gates.as_slice() else {
        return Err(Fault("ruleset_gate_missing_or_ambiguous"));
    };
    let held = |name: &str, text: &str| Ok::<_, Fault>(is(Some(field(gate, name)?), text));
    let passed = held("head_sha", revision)?
        && held("status", "completed")?
        && held("conclusion", "success")?
        && field(object(field(gate, "app")?)?, "id")?.as_u64() == Some(GITHUB_ACTIONS);
    require(passed, "ruleset_gate_not_successful")
}

/// The newest run of the trusted workflow for `revision` passed: a push to
/// `main`, completed with success.
fn healthy_run(answer: &Value, revision: &str) -> Result<()> {
    let runs = records(field(object(answer)?, "workflow_runs")?)?;
    let mut newest: Option<(u64, &Map<String, Value>)> = None;
    for run in runs {
        let number = field(run, "id")?.as_u64().ok_or(ANSWER_FORMAT)?;
        if newest.is_none_or(|(highest, _)| number > highest) {
            newest = Some((number, run));
        }
    }
    let (_, run) = newest.ok_or(Fault("ruleset_trusted_ci_missing"))?;
    let held = |name: &str, text: &str| Ok::<_, Fault>(is(Some(field(run, name)?), text));
    let passed = held("head_sha", revision)?
        && held("head_branch", "main")?
        && held("event", "push")?
        && held("status", "completed")?
        && held("conclusion", "success")?;
    require(passed, "ruleset_trusted_ci_not_successful")
}

/// Every required language has an analysis, and the newest of each is of
/// `revision` on `main`, without error or warning, and ran rules. The
/// answer lists the newest first.
fn healthy_analyses(answer: &Value, revision: &str) -> Result<()> {
    let entries = records(answer)?;
    require(entries.len() <= PAGE, "ruleset_analysis_window")?;
    let mut newest: Vec<(&str, &Map<String, Value>)> = Vec::new();
    for entry in entries {
        if !is(object(field(entry, "tool")?)?.get("name"), "CodeQL") {
            continue;
        }
        let category = field(entry, "category")?.as_str().ok_or(ANSWER_FORMAT)?;
        let language = category
            .strip_prefix("/language:")
            .and_then(|rest| rest.strip_suffix("/security"));
        let required = language.is_some_and(|language| LANGUAGES.contains(&language));
        if required && newest.iter().all(|(seen, _)| *seen != category) {
            newest.push((category, entry));
        }
    }
    require(newest.len() == LANGUAGES.len(), "ruleset_language_coverage")?;
    for (_, entry) in newest {
        require(
            is(entry.get("commit_sha"), revision) && is(entry.get("ref"), "refs/heads/main"),
            "ruleset_analysis_not_current",
        )?;
        require(
            is(entry.get("error"), "") && is(entry.get("warning"), ""),
            "ruleset_analysis_failed",
        )?;
        let rules = entry.get("rules_count").and_then(Value::as_u64);
        require(
            rules.is_some_and(|rules| rules > 0),
            "ruleset_analysis_empty",
        )?;
    }
    Ok(())
}

/// Where the two programs are looked for.
pub type Programs<'a> = &'a dyn Fn(&'static str) -> Outcome<PathBuf>;

/// The questions of one run: the checkout they are asked from, where the
/// programs are, and the repository they are about.
struct Asking<'a> {
    root: &'a Path,
    programs: Programs<'a>,
    repository: &'a str,
}

impl Asking<'_> {
    /// What `program` prints for `arguments` when it ends with the status
    /// 0, having read `input`. It runs in the caller's environment.
    fn command(&self, program: &'static str, arguments: &[&str], input: &[u8]) -> Outcome<Vec<u8>> {
        let program = (self.programs)(program)?;
        let mut command = Command::new(program);
        command.args(arguments).current_dir(self.root);
        let (status, printed) = context::asked(&mut command, input, COMMAND_TIME, ROOM)?;
        require(status == 0, "ruleset_command_failed")?;
        Ok(printed)
    }

    /// What GitHub answers at `path` of the repository. A `body` is sent
    /// on standard input: it replaces the ruleset the path names, or is
    /// added where the path names none.
    fn api(&self, path: &str, body: Option<&Map<String, Value>>) -> Outcome<Value> {
        let address = format!("repos/{}/{path}", self.repository);
        let address = address.trim_end_matches('/');
        let printed = match body {
            None => self.command("gh", &["api", address], &[])?,
            Some(body) => {
                let replaces = path.starts_with("rulesets/");
                let method = if replaces { "PUT" } else { "POST" };
                let sent = Value::Object(body.clone()).to_string();
                let arguments = ["api", address, "--method", method, "--input", "-"];
                self.command("gh", &arguments, sent.as_bytes())?
            }
        };
        Ok(record::strict(&printed).map_err(|_| ANSWER_FORMAT)?)
    }

    /// The managed ruleset and the obsolete ones that the repository itself
    /// holds, each read in full, in the order of their names. A full page
    /// may not be all there is, and a name held twice names nothing.
    fn inventory(&self) -> Outcome<Vec<(String, Map<String, Value>)>> {
        let listed = self.api("rulesets?includes_parents=false&per_page=100", None)?;
        let listed = listed.as_array().ok_or(ANSWER_FORMAT)?;
        require(listed.len() < PAGE, "ruleset_inventory_limit")?;
        let mut held: Vec<(String, Map<String, Value>)> = Vec::new();
        for entry in listed {
            let entry = object(entry)?;
            let name = field(entry, "name")?.as_str().ok_or(ANSWER_FORMAT)?;
            if name != NAME && !OBSOLETE.contains(&name) {
                continue;
            }
            let number = field(entry, "id")?.as_u64().filter(|number| *number > 0);
            let once = held.iter().all(|(seen, _)| seen != name);
            let number = number.filter(|_| once).ok_or(Fault("ruleset_ambiguous"))?;
            let whole = self.api(&format!("rulesets/{number}"), None)?;
            held.push((name.to_owned(), object(&whole)?.clone()));
        }
        held.sort_by(|(left, _), (right, _)| left.cmp(right));
        Ok(held)
    }

    /// `main` is the commit `revision`, and nothing else by that name.
    fn head(&self, revision: &str) -> Outcome<()> {
        let answer = self.api("git/ref/heads/main", None)?;
        let answer = object(&answer)?;
        let target = object(field(answer, "object")?)?;
        let current = is(answer.get("ref"), "refs/heads/main")
            && is(target.get("type"), "commit")
            && is(target.get("sha"), revision);
        Ok(require(current, "ruleset_remote_head_changed")?)
    }

    /// No open alert of critical or high severity is in maintained code.
    /// One in vendored or native code is left to its own optional review.
    fn alerts(&self) -> Outcome<()> {
        for severity in ["critical", "high"] {
            let mut page = 1;
            loop {
                require(page <= PAGE, "ruleset_alert_page_limit")?;
                let path = format!(
                    "code-scanning/alerts?state=open&severity={severity}\
                     &ref=refs%2Fheads%2Fmain&per_page=100&page={page}"
                );
                let answer = self.api(&path, None)?;
                let alerts = answer.as_array().ok_or(ANSWER_FORMAT)?;
                for alert in alerts {
                    let instance = object(field(object(alert)?, "most_recent_instance")?)?;
                    let place = object(field(instance, "location")?)?;
                    let path = field(place, "path")?.as_str().ok_or(ANSWER_FORMAT)?;
                    let category = field(instance, "category")?.as_str();
                    let category = category.ok_or(ANSWER_FORMAT)?;
                    let optional =
                        path.starts_with("vendor/") || category.starts_with("/language:c-cpp/");
                    require(optional, "ruleset_open_high_security_alerts_need_review")?;
                }
                if alerts.len() < PAGE {
                    break;
                }
                page += 1;
            }
        }
        Ok(())
    }

    /// The checkout is clean and at `revision`, `main` is at it too, and
    /// everything that gates a release passed for it.
    fn ready(&self, revision: &str) -> Outcome<()> {
        require(commit(revision), "ruleset_revision")?;
        let head = self.command("git", &["rev-parse", "HEAD"], &[])?;
        let head = String::from_utf8(head).map_err(|_| ANSWER_FORMAT)?;
        require(head.trim() == revision, "ruleset_local_head")?;
        let changed = self.command(
            "git",
            &["status", "--porcelain=v1", "--untracked-files=all"],
            &[],
        )?;
        require(changed.is_empty(), "ruleset_dirty_checkout")?;
        self.head(revision)?;
        let checks = format!("commits/{revision}/check-runs?per_page=100&filter=latest");
        healthy_checks(&self.api(&checks, None)?, revision)?;
        let runs =
            format!("actions/workflows/ci.yml/runs?head_sha={revision}&event=push&per_page=10");
        healthy_run(&self.api(&runs, None)?, revision)?;
        let analyses = "code-scanning/analyses?ref=refs%2Fheads%2Fmain&per_page=100\
                        &tool_name=CodeQL&sort=created&direction=desc";
        healthy_analyses(&self.api(analyses, None)?, revision)?;
        self.alerts()
    }
}

/// What a run found: whether the managed ruleset differs from the policy,
/// and which obsolete rulesets the repository still holds.
struct Found {
    repository: String,
    changed: bool,
    obsolete: Vec<String>,
}

/// The obsolete rulesets among those held, and the managed one.
fn sorted(held: &[(String, Map<String, Value>)]) -> (Vec<String>, Option<&Map<String, Value>>) {
    let obsolete = held.iter().filter(|(name, _)| name != NAME);
    let managed = held.iter().find(|(name, _)| name == NAME);
    (
        obsolete.map(|(name, _)| name.clone()).collect(),
        managed.map(|(_, ruleset)| ruleset),
    )
}

/// Compare the repository's rulesets with the policy and, to apply it,
/// write the managed ruleset and read it back.
fn reconciled(
    root: &Path,
    programs: Programs<'_>,
    mode: Mode,
    revision: Option<&str>,
) -> Outcome<Found> {
    let policy = loaded(root)?;
    let asking = Asking {
        root,
        programs,
        repository: &policy.repository,
    };
    let repository = asking.api("", None)?;
    let repository = object(&repository)?;
    let same = field(repository, "id")?.as_u64() == Some(policy.identifier)
        && is(Some(field(repository, "default_branch")?), "main");
    require(same, "ruleset_repository_changed")?;
    let wanted = comparable(&policy.ruleset)?;
    let held = asking.inventory()?;
    let (mut obsolete, managed) = sorted(&held);
    let changed = match managed {
        Some(managed) => comparable(managed)? != wanted,
        None => true,
    };
    if mode == Mode::Apply {
        require(obsolete.is_empty(), "ruleset_obsolete_policy_present")?;
        let revision = revision.ok_or(Fault("ruleset_revision_required"))?;
        asking.ready(revision)?;
        if changed {
            asking.head(revision)?;
            let place = match managed {
                Some(managed) => {
                    let number = field(managed, "id")?.as_u64().ok_or(ANSWER_FORMAT)?;
                    format!("rulesets/{number}")
                }
                None => "rulesets".to_owned(),
            };
            asking.api(&place, Some(&policy.ruleset))?;
        }
        asking.head(revision)?;
        let held = asking.inventory()?;
        asking.head(revision)?;
        let (still, managed) = sorted(&held);
        obsolete = still;
        require(obsolete.is_empty(), "ruleset_obsolete_policy_present")?;
        let read = comparable(managed.ok_or(ANSWER_FORMAT)?)?;
        require(read == wanted, "ruleset_readback_differs")?;
    }
    Ok(Found {
        repository: policy.repository,
        changed,
        obsolete,
    })
}

/// What one run answers: what it prints, what it complains of, and the
/// status it ends with.
#[derive(Debug, PartialEq, Eq)]
pub struct Answer {
    /// One line of JSON saying what was found, or nothing after a failure.
    pub output: String,
    /// One line naming the failure's code, or nothing.
    pub errors: String,
    /// 0, or 1 after a failure and when a check finds a difference.
    pub status: u8,
}

/// Run the command in the checkout at `root`. A check that finds the
/// managed ruleset different or an obsolete one present ends with 1 and
/// still says what it found.
pub fn reconcile(
    root: &Path,
    programs: Programs<'_>,
    mode: Mode,
    revision: Option<&str>,
) -> Answer {
    let found = match reconciled(root, programs, mode, revision) {
        Ok(found) => found,
        Err(failure) => {
            return Answer {
                output: String::new(),
                errors: format!("Ruleset reconciliation failed: {failure}\n"),
                status: 1,
            };
        }
    };
    let changes = if found.changed {
        quoted(NAME)
    } else {
        String::new()
    };
    let obsolete: Vec<String> = found.obsolete.iter().map(|name| quoted(name)).collect();
    let matches = obsolete.is_empty() && (!found.changed || mode == Mode::Apply);
    let differs = found.changed || !obsolete.is_empty();
    Answer {
        output: format!(
            "{{\"mode\": {}, \"repository\": {}, \"changes\": [{changes}], \
             \"obsoleteRulesets\": [{}], \"matches\": {matches}}}\n",
            quoted(mode.name()),
            quoted(&found.repository),
            obsolete.join(", "),
        ),
        errors: String::new(),
        status: u8::from(mode == Mode::Check && differs),
    }
}

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Asked<'a> {
    /// One run, with the revision it names or none.
    Run(Mode, Option<&'a str>),
    /// How the command is used.
    Help,
}

const USAGE: &str = "usage: simplestchat-security rulesets plan|check|apply [--revision COMMIT]";

/// Read the command line: one mode, and a revision given after
/// `--revision` or joined to it by an equals sign. Of two revisions the
/// last is taken, and one given after the option begins with no dash.
fn asked(arguments: &[String]) -> Option<Asked<'_>> {
    let (mut mode, mut revision) = (None, None);
    let mut rest = arguments.iter().map(String::as_str);
    while let Some(argument) = rest.next() {
        let named = match argument {
            "plan" => Some(Mode::Plan),
            "check" => Some(Mode::Check),
            "apply" => Some(Mode::Apply),
            _ => None,
        };
        if argument == "--help" || argument == "-h" {
            return Some(Asked::Help);
        } else if let Some(named) = named {
            if mode.replace(named).is_some() {
                return None;
            }
        } else if argument == "--revision" {
            revision = Some(rest.next().filter(|value| !value.starts_with('-'))?);
        } else {
            revision = Some(argument.strip_prefix("--revision=")?);
        }
    }
    Some(Asked::Run(mode?, revision))
}

/// Run one `rulesets` command for the checkout at `root`, with the two
/// programs looked for in the directories of `search`.
pub fn run(root: &Path, arguments: &[String], search: Option<&std::ffi::OsStr>) -> ExitCode {
    let (mode, revision) = match asked(arguments) {
        Some(Asked::Run(mode, revision)) => (mode, revision),
        Some(Asked::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        None => {
            eprintln!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    let programs = |name: &'static str| context::executable(name, search);
    let answer = reconcile(root, &programs, mode, revision);
    let printed = std::io::stdout().write_all(answer.output.as_bytes());
    let complained = std::io::stderr().write_all(answer.errors.as_bytes());
    match printed.and(complained) {
        Ok(()) => ExitCode::from(answer.status),
        Err(_) => ExitCode::from(1),
    }
}

#[cfg(test)]
mod tests;
