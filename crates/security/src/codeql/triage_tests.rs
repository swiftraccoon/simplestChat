//! Replays of `recorded_triage.json`: what `build/security_codeql_triage.py`
//! made of values, scores, locations, reports, committed and native
//! sources, findings, alerts and reviews, each case rebuilt in its own
//! place and read through a `git show` that serves the recorded commit;
//! and what the triage replays share.

use super::{
    Ledger, digest,
    findings::{finding, source_hash, verdict},
    github::Clock,
    plan::{Checkout, api_finding, false_positive, policy_identity, review_identity},
    positive,
    sarif::{first_party_report, sarif_findings, sarif_location, sarif_score},
};
use crate::{
    Result,
    codeql::{
        Sourced, Stop,
        sources::{NativeSources, Shown},
        tests::{bytes, items, planted, recording, text},
    },
    document::Document,
    policy::{Date, Exception},
    scratch::Scratch,
    vendor::Bounds,
};
use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    path::{Path, PathBuf},
};

const RECORDED: &[u8] = include_bytes!("recorded_triage.json");

/// A clock that moves only when it is told to, or when it is asked to wait.
pub(crate) struct Virtual {
    now: Cell<f64>,
    tick: f64,
    sleeps: RefCell<Vec<f64>>,
}

impl Virtual {
    /// A clock at `start`.
    pub(crate) fn new(start: f64) -> Self {
        Self::ticking(start, 0.0)
    }

    /// A clock at `start` that moves on by `tick` once it has been read, as
    /// the reference's was in the recorded case: every reading counts.
    pub(crate) fn ticking(start: f64, tick: f64) -> Self {
        Self {
            now: Cell::new(start),
            tick,
            sleeps: RefCell::new(Vec::new()),
        }
    }

    /// The time, read without moving the clock on.
    pub(crate) fn time(&self) -> f64 {
        self.now.get()
    }

    /// Move the clock on by `seconds`.
    pub(crate) fn advance(&self, seconds: f64) {
        self.now.set(self.now.get() + seconds);
    }

    /// Every wait asked of it, in order.
    pub(crate) fn sleeps(&self) -> Document {
        let sleeps = self.sleeps.borrow();
        Document::List(sleeps.iter().map(|seconds| number(*seconds)).collect())
    }
}

impl Clock for Virtual {
    fn now(&self) -> f64 {
        let now = self.now.get();
        self.advance(self.tick);
        now
    }

    fn sleep(&self, seconds: f64) {
        let mut sleeps = self.sleeps.borrow_mut();
        // No recorded case waits more than a few hundred times; a pacing
        // that never ends is a failure, not a test that never ends.
        assert!(sleeps.len() < 10_000, "a wait that never ends");
        sleeps.push(seconds);
        drop(sleeps);
        self.advance(seconds);
    }
}

/// `value` as a document's number.
pub(crate) fn number(value: f64) -> Document {
    Document::Number(serde_json::Number::from_f64(value).expect("a finite number"))
}

/// The number `name` of `value`, whole or not.
pub(crate) fn seconds(value: &Document, name: &str) -> f64 {
    let held = value.get(name).and_then(|held| match held {
        Document::Number(number) => number.as_f64(),
        _ => None,
    });
    held.unwrap_or_else(|| panic!("no number {name}"))
}

/// A `git show` of the recorded commit of the revision `revision`: an
/// object of another revision, or of a file it does not hold, ends with
/// 1; one past the bound is refused as the reference's bounded runner
/// refused it; any other ends with the status the recording gives it. A
/// hook changes the checkout when its object is asked for the time it
/// names. Each show takes `seconds` of the clock's time.
pub(crate) struct Served<'a> {
    root: PathBuf,
    revision: String,
    tracked: RefCell<BTreeMap<String, Vec<u8>>>,
    statuses: BTreeMap<String, i32>,
    hooks: RefCell<Vec<(Document, bool)>>,
    counts: RefCell<BTreeMap<String, i128>>,
    asked: RefCell<Vec<String>>,
    clock: &'a Virtual,
    seconds: f64,
}

impl<'a> Served<'a> {
    /// The commit `tracked` of the checkout `root`, with the hooks `hooks`.
    pub(crate) fn new(
        root: &Path,
        revision: &str,
        tracked: &Document,
        hooks: &Document,
        clock: &'a Virtual,
        seconds: f64,
    ) -> Self {
        let entries = tracked.fields().expect("a commit");
        let entries = entries.iter().filter(|(_, spec)| *spec != Document::Null);
        let tracked = entries
            .clone()
            .map(|(name, spec)| (name.clone(), bytes(spec, root)))
            .collect();
        let statuses = entries
            .filter_map(|(name, spec)| {
                let status = spec.get("status").and_then(Document::whole)?;
                Some((name.clone(), i32::try_from(status).expect("a status")))
            })
            .collect();
        let hooks = hooks.items().unwrap_or_default();
        Self {
            root: root.to_path_buf(),
            revision: revision.to_owned(),
            tracked: RefCell::new(tracked),
            statuses,
            hooks: RefCell::new(hooks.iter().map(|hook| (hook.clone(), false)).collect()),
            counts: RefCell::new(BTreeMap::new()),
            asked: RefCell::new(Vec::new()),
            clock,
            seconds,
        }
    }

    /// What it was asked, in order, since it was last cleared.
    pub(crate) fn asked(&self) -> Document {
        let asked = self.asked.borrow();
        Document::List(asked.iter().map(|object| object.as_str().into()).collect())
    }

    /// Forget what it was asked.
    pub(crate) fn clear(&self) {
        self.asked.borrow_mut().clear();
    }
}

impl Shown for Served<'_> {
    fn show(&self, root: &Path, object: &str, limit: u64) -> Sourced<(i32, Vec<u8>)> {
        assert_eq!(root, self.root, "git runs in the checkout");
        self.asked.borrow_mut().push(object.to_owned());
        let count = {
            let mut counts = self.counts.borrow_mut();
            let count = counts.entry(object.to_owned()).or_insert(0);
            *count += 1;
            *count
        };
        for (hook, done) in self.hooks.borrow_mut().iter_mut() {
            let when = hook.get("when").and_then(Document::text) == Some(object);
            let time = hook.get("count").and_then(Document::whole).unwrap_or(1);
            if when && time == count && !*done {
                *done = true;
                if let Some(write) = hook.get("write") {
                    planted(root, write);
                }
                for (name, spec) in hook
                    .get("track")
                    .and_then(Document::fields)
                    .unwrap_or_default()
                {
                    self.tracked
                        .borrow_mut()
                        .insert(name.clone(), bytes(spec, root));
                }
            }
        }
        self.clock.advance(self.seconds);
        let (revision, name) = object.split_once(':').expect("an object");
        let tracked = self.tracked.borrow();
        let Some(data) = tracked.get(name).filter(|_| revision == self.revision) else {
            return Ok((1, Vec::new()));
        };
        if data.len() as u64 > limit {
            return Err(Stop::Code("command_stdout_limit_exceeded"));
        }
        Ok((self.statuses.get(name).copied().unwrap_or(0), data.clone()))
    }
}

/// The reviews the recording lists, each complete, current and owned.
pub(crate) fn exceptions(reviews: &Document) -> Vec<Exception> {
    let reviews = reviews.items().expect("reviews");
    reviews
        .iter()
        .map(|review| Exception {
            scanner: text(review, "scanner").to_owned(),
            fingerprint: text(review, "fingerprint").to_owned(),
            scope: text(review, "scope").to_owned(),
            owner: "fixture".to_owned(),
            rationale: "r".to_owned(),
            reachability: "r".to_owned(),
            expires: Date::new(2099, 1, 1).expect("a day"),
            review: "https://example.test/r".to_owned(),
        })
        .collect()
}

/// `value` with the place `place` written as the recording writes it.
pub(crate) fn scrubbed(value: &Document, place: &Path) -> Document {
    fn written(value: &Document, place: &str) -> Document {
        match value {
            Document::Text(text) => text.replace(place, "<place>").into(),
            Document::List(items) => {
                Document::List(items.iter().map(|item| written(item, place)).collect())
            }
            Document::Record(fields) => Document::Record(
                fields
                    .iter()
                    .map(|(name, item)| (name.clone(), written(item, place)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    written(value, &place.to_string_lossy())
}

/// How the recorded cases came out here: held alike, refused with the
/// reference's code, refused with the class its interpreter raised, and
/// refused on purpose where the reference held.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Count {
    pub held: usize,
    pub refused: usize,
    pub raised: usize,
    pub stricter: usize,
}

impl Count {
    /// Hold what came of the case `name` here to its recorded result
    /// `recorded`: a held value exactly, in its order, or only by its digest;
    /// a refusal by its code; what the reference's interpreter raised by its
    /// class. `stricter` names the cases the reference held that are
    /// refused here on purpose, each with its refusal.
    pub(crate) fn judge(
        &mut self,
        name: &str,
        recorded: &Document,
        here: &Result<Document>,
        stricter: &[(&str, &str)],
    ) {
        let here = here.as_ref().map_err(|fault| fault.0);
        if let Some((_, refusal)) = stricter.iter().find(|(held, _)| *held == name) {
            assert!(recorded.get("held").is_some(), "{name} was held there");
            assert_eq!(here, Err(*refusal), "{name}");
            self.stricter += 1;
        } else if let Some(held) = recorded.get("held") {
            assert_eq!(here, Ok(held), "{name}");
            self.held += 1;
        } else if let Some(hash) = recorded.get("heldDigest").and_then(Document::text) {
            let came = here.unwrap_or_else(|refusal| panic!("{name}: refused {refusal}"));
            assert_eq!(digest(came), hash, "{name}");
            self.held += 1;
        } else if let Some(code) = recorded.get("refused").and_then(Document::text) {
            assert_eq!(here, Err(code), "{name}");
            self.refused += 1;
        } else {
            let class = text(recorded, "raised");
            assert_eq!(here, Err(class), "{name}");
            self.raised += 1;
        }
    }

    /// The tally as `[held, refused, raised, stricter]`.
    pub(crate) fn tally(&self) -> [usize; 4] {
        [self.held, self.refused, self.raised, self.stricter]
    }
}

/// A fresh place holding the tree of `case`.
pub(crate) fn placed(case: &Document) -> Scratch {
    let scratch = Scratch::new("codeql-triage");
    planted(&scratch.0, case.get("tree").expect("a tree"));
    scratch
}

/// The recorded revision.
fn revision(recorded: &Document) -> &str {
    text(recorded, "revision")
}

/// The native sources of the checkout `root` through its own cache, as the
/// recorder made them.
fn native<'a>(root: &Path, revision: &str, git: &'a Served<'_>) -> NativeSources<'a> {
    let made = NativeSources::new(root, revision, &root.join("cache"), git, Bounds::REVIEWED);
    let made = made.unwrap_or_else(|stop| panic!("native sources: {stop}"));
    git.clear();
    made
}

/// The case is native, as the recording says.
fn is_native(case: &Document) -> bool {
    case.get("native") == Some(&Document::Bool(true))
}

/// The cases whose value is an integer past 64 bits: the reference's
/// interpreter held any integer exactly, and this reader keeps the
/// fraction nearest it.
const WIDE: [(&str, &str); 2] = [
    ("1.8446744073709552e+19", "codeql_positive_integer"),
    ("1e+30", "codeql_positive_integer"),
];

#[test]
fn every_recorded_value_is_held_to_be_positive_alike() {
    let recorded = recording(RECORDED);
    let mut count = Count::default();
    for case in items(&recorded, "positives") {
        let value = case.get("value").expect("a value");
        let here = positive(value).map(Document::from);
        count.judge(
            &value.compact(),
            case.get("result").expect("a result"),
            &here,
            &WIDE,
        );
    }
    assert_eq!(count.tally(), [5, 13, 0, 2]);
}

#[test]
fn every_recorded_identity_is_digested_alike() {
    let recorded = recording(RECORDED);
    let cases = items(&recorded, "digests");
    for case in cases {
        let value = case.get("value").expect("a value");
        assert_eq!(digest(value), text(case, "digest"), "{}", value.compact());
    }
    assert_eq!(cases.len(), 11);
}

#[test]
fn every_recorded_rule_is_scored_alike() {
    let recorded = recording(RECORDED);
    let mut count = Count::default();
    for case in items(&recorded, "scores") {
        let rule = case.get("rule").expect("a rule");
        let here = sarif_score(rule).map(Document::from);
        count.judge(
            &rule.compact(),
            case.get("result").expect("a result"),
            &here,
            &[],
        );
    }
    assert_eq!(count.tally(), [29, 17, 8, 0]);
}

#[test]
fn every_recorded_physical_location_is_read_alike() {
    let recorded = recording(RECORDED);
    let mut count = Count::default();
    for case in items(&recorded, "locations") {
        let physical = case.get("physical").expect("a location");
        let here = sarif_location(physical);
        let name = physical.compact();
        count.judge(&name, case.get("result").expect("a result"), &here, &[]);
    }
    assert_eq!(count.tally(), [37, 7, 5, 0]);
}

#[test]
fn every_recorded_report_is_held_to_its_first_party_sources_alike() {
    let recorded = recording(RECORDED);
    let mut count = Count::default();
    for case in items(&recorded, "firstParty") {
        let raw = case.get("raw").expect("a report");
        let here = first_party_report(raw).map(|()| Document::Null);
        count.judge(
            text(case, "name"),
            case.get("result").expect("a result"),
            &here,
            &[],
        );
    }
    assert_eq!(count.tally(), [5, 5, 8, 0]);
}

#[test]
fn every_recorded_source_is_hashed_alike() {
    let recorded = recording(RECORDED);
    let mut count = Count::default();
    for case in items(&recorded, "sourceHashes") {
        let scratch = placed(case);
        let place = &scratch.0;
        let root = PathBuf::from(text(case, "root").replace("<place>", &place.to_string_lossy()));
        let clock = Virtual::new(0.0);
        let git = Served::new(
            &root,
            revision(&recorded),
            case.get("tracked").expect("a commit"),
            &Document::Null,
            &clock,
            0.0,
        );
        let here = source_hash(&root, text(case, "revision"), text(case, "path"), &git);
        let name = text(case, "name");
        count.judge(
            name,
            case.get("result").expect("a result"),
            &here.map(Document::from),
            &[],
        );
        assert_eq!(&git.asked(), case.get("asked").expect("asked"), "{name}");
    }
    assert_eq!(count.tally(), [7, 36, 6, 0]);
}

#[test]
fn every_recorded_finding_is_identified_alike() {
    let recorded = recording(RECORDED);
    let mut count = Count::default();
    for case in items(&recorded, "findings") {
        let scratch = placed(case);
        let place = &scratch.0;
        let clock = Virtual::new(0.0);
        let tracked = case.get("tracked").expect("a commit");
        let git = Served::new(
            place,
            revision(&recorded),
            tracked,
            &Document::Null,
            &clock,
            0.0,
        );
        let mut sources = is_native(case).then(|| native(place, revision(&recorded), &git));
        let here = finding(
            place,
            text(case, "revision"),
            text(case, "rule"),
            text(case, "version"),
            case.get("location").expect("a location"),
            text(case, "message"),
            sources.as_mut(),
            &git,
        );
        let name = text(case, "name");
        let here = here.map(|found| scrubbed(&found, place));
        count.judge(name, case.get("result").expect("a result"), &here, &[]);
        assert_eq!(&git.asked(), case.get("asked").expect("asked"), "{name}");
    }
    assert_eq!(count.tally(), [59, 14, 6, 0]);
}

#[test]
fn every_recorded_verdict_is_reached_alike() {
    let recorded = recording(RECORDED);
    let mut count = Count::default();
    for case in items(&recorded, "verdicts") {
        let findings = items(case, "findings");
        let reviews = exceptions(case.get("reviews").expect("reviews"));
        let here = verdict(findings, &reviews);
        count.judge(
            text(case, "name"),
            case.get("result").expect("a result"),
            &here,
            &[],
        );
    }
    assert_eq!(count.tally(), [9, 3, 5, 0]);
}

/// The report of `case`: its runs' results given as many times as it says.
fn report(case: &Document) -> Document {
    let mut raw = case.get("raw").expect("a report").clone();
    let Some(copies) = case.get("copies").and_then(Document::items) else {
        return raw;
    };
    let runs = items(&raw, "runs").iter().zip(copies).map(|(run, copies)| {
        let times = usize::try_from(copies.whole().expect("a count")).expect("a count");
        let results = items(run, "results");
        let repeated = results.iter().cycle().take(results.len() * times).cloned();
        let mut run = run.clone();
        run.put("results", Document::List(repeated.collect()));
        run
    });
    let runs = Document::List(runs.collect());
    raw.put("runs", runs);
    raw
}

#[test]
fn every_recorded_sarif_report_is_read_alike() {
    let recorded = recording(RECORDED);
    let mut count = Count::default();
    for case in items(&recorded, "sarifs") {
        let scratch = placed(case);
        let place = &scratch.0;
        let clock = Virtual::ticking(1000.0, seconds(case, "tick"));
        let tracked = case.get("tracked").expect("a commit");
        let pace = seconds(case, "gitSeconds");
        let git = Served::new(
            place,
            revision(&recorded),
            tracked,
            &Document::Null,
            &clock,
            pace,
        );
        let mut sources = is_native(case).then(|| native(place, revision(&recorded), &git));
        let raw = report(case);
        let here = sarif_findings(
            &raw,
            place,
            revision(&recorded),
            sources.as_mut(),
            &git,
            &clock,
        );
        let summary = case.get("summary") == Some(&Document::Bool(true));
        let here = here.map(|found| {
            if summary {
                let each = found.first().cloned().unwrap_or(Document::Null);
                assert!(found.iter().all(|item| *item == each), "alike findings");
                Document::record([("count", found.len().into()), ("each", each)])
            } else {
                scrubbed(&Document::List(found), place)
            }
        });
        let name = text(case, "name");
        count.judge(name, case.get("result").expect("a result"), &here, &[]);
        let asked = case.get("asked").expect("asked");
        if summary {
            assert_eq!(
                Some(git.asked().items().unwrap_or_default().len() as i128),
                asked.whole()
            );
        } else {
            assert_eq!(&git.asked(), asked, "{name}");
        }
    }
    assert_eq!(count.tally(), [42, 71, 27, 0]);
}

#[test]
fn every_recorded_alert_is_identified_alike() {
    let recorded = recording(RECORDED);
    let mut count = Count::default();
    for case in items(&recorded, "apiFindings") {
        let scratch = placed(case);
        let place = &scratch.0;
        let clock = Virtual::new(0.0);
        let tracked = case.get("tracked").expect("a commit");
        let git = Served::new(
            place,
            revision(&recorded),
            tracked,
            &Document::Null,
            &clock,
            0.0,
        );
        let mut native_sources = is_native(case).then(|| native(place, revision(&recorded), &git));
        let mut checkout = Checkout {
            root: place,
            sources: native_sources.as_mut(),
            git: &git,
        };
        let raw = case.get("raw").expect("an alert");
        let here = api_finding(
            raw,
            &mut checkout,
            revision(&recorded),
            text(case, "reference"),
        );
        let name = text(case, "name");
        let here = here.map(|found| scrubbed(&found, place));
        count.judge(name, case.get("result").expect("a result"), &here, &[]);
        assert_eq!(&git.asked(), case.get("asked").expect("asked"), "{name}");
    }
    assert_eq!(count.tally(), [18, 15, 26, 0]);
}

#[test]
fn every_recorded_review_is_decided_alike() {
    let recorded = recording(RECORDED);
    let mut count = Count::default();
    let mut identities = Count::default();
    for case in items(&recorded, "reviews") {
        let scratch = placed(case);
        let ledger = Ledger::of(&scratch.0);
        let item = case.get("item").expect("an item");
        let here = false_positive(item, text(case, "repository"), &ledger).map(Document::from);
        let name = text(case, "name");
        count.judge(name, case.get("result").expect("a result"), &here, &[]);
        let policy = policy_identity(&ledger).map(Document::from);
        identities.judge(name, case.get("policy").expect("a policy"), &policy, &[]);
        let review = review_identity(&ledger).map(Document::from);
        identities.judge(name, case.get("review").expect("a review"), &review, &[]);
    }
    assert_eq!(count.tally(), [25, 8, 13, 0]);
    assert_eq!(identities.tally(), [84, 3, 5, 0]);
}
