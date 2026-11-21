//! The zizmor reviews of the committed policy, held to the workflows they
//! name.
//!
//! A zizmor review is bound to a place, `<file>#<route>`, and to the SHA-256
//! of the feature that was flagged there. The gate compares a review with
//! what the scanner reports; these tests compare it with the file itself, so
//! that a reviewed step which moved, or whose text changed, is found without
//! the scanner and also when the scanner no longer flags it.
//!
//! A route is the keys and indexes from the top of the document, joined by
//! `/`; a part of ASCII digits only is an index. The feature of a scalar is
//! its value. The feature of a mapping is its source text, from the key that
//! the route ends with (or from the mapping, when it ends with an index) to
//! where the mapping ends, without the white space before that end: comments
//! between the mapping and what follows it are part of the feature. A list
//! has no feature.

use crate::{
    Fault, Result, digest, files,
    policy::{self, Date, Exception, space},
    record::{MAX_DOCUMENT, safe_name},
    require,
    scratch::Scratch,
};
use serde_saphyr::granit_parser::{Event, Options, Parser, ScalarStyle, Span};
use std::path::Path;

/// A scope or a fingerprint that is not of the two parts a zizmor review has.
const PLACE: Fault = Fault("workflow_review_place");
/// A file that is no single YAML document of plain mappings, lists and
/// scalars: an anchor, an alias, a tag or a key that is no scalar is refused,
/// as is nesting beyond [`MAX_DEPTH`].
const YAML: Fault = Fault("workflow_review_yaml");
/// A route that leads nowhere in the document, or to more than one key.
const ROUTE: Fault = Fault("workflow_review_route");
/// A route that leads to a list.
const FEATURE: Fault = Fault("workflow_review_feature");
/// A feature that is not the one the review's fingerprint names.
const CHANGED: Fault = Fault("workflow_review_feature_changed");
/// The deepest nesting of one workflow.
const MAX_DEPTH: usize = 64;

/// One part of a route.
#[derive(Debug, PartialEq, Eq)]
enum Step {
    /// The value of this key of a mapping.
    Key(String),
    /// The item at this place of a list, counted from zero.
    Index(usize),
}

/// The steps of the route `text`. A part of digits beyond what an index can
/// hold is no step.
fn route(text: &str) -> Result<Vec<Step>> {
    text.split('/')
        .map(|part| {
            if !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()) {
                part.parse().map(Step::Index).map_err(|_| ROUTE)
            } else {
                Ok(Step::Key(part.to_owned()))
            }
        })
        .collect()
}

/// An event of a document and where its source is.
type Parsed<'a> = (Event<'a>, Span);

/// What a node of a document is.
#[derive(Debug)]
enum Kind {
    Scalar(String),
    List(Vec<Node>),
    Map(Vec<(Node, Node)>),
}

/// A node and where its source is, in characters from the start of the text.
/// A block mapping ends where the next token after it starts.
#[derive(Debug)]
struct Node {
    kind: Kind,
    start: usize,
    end: usize,
}

/// The next event of a document that has not ended.
fn next<'a>(events: &mut impl Iterator<Item = Result<Parsed<'a>>>) -> Result<Parsed<'a>> {
    events.next().ok_or(YAML)?
}

/// The node that `first` starts, with the events it needs from `rest`.
fn node<'a>(
    first: Parsed<'a>,
    rest: &mut impl Iterator<Item = Result<Parsed<'a>>>,
    depth: usize,
) -> Result<Node> {
    require(depth <= MAX_DEPTH, YAML.0)?;
    let (event, span) = first;
    let start = span.start.index();
    let (kind, end) = match event {
        Event::Scalar(value, style, 0, None) => {
            // A value that is not written is read as `~`: it is no text.
            let unwritten = style == ScalarStyle::Plain && start == span.end.index();
            let value = if unwritten {
                String::new()
            } else {
                value.into_owned()
            };
            (Kind::Scalar(value), span.end)
        }
        Event::SequenceStart(_, 0, None) => {
            let mut items = Vec::new();
            loop {
                let (event, span) = next(rest)?;
                if event == Event::SequenceEnd {
                    break (Kind::List(items), span.end);
                }
                items.push(node((event, span), rest, depth + 1)?);
            }
        }
        Event::MappingStart(_, 0, None) => {
            let mut entries = Vec::new();
            loop {
                let (event, span) = next(rest)?;
                if event == Event::MappingEnd {
                    break (Kind::Map(entries), span.end);
                }
                let key = node((event, span), rest, depth + 1)?;
                require(matches!(key.kind, Kind::Scalar(_)), YAML.0)?;
                entries.push((key, node(next(rest)?, rest, depth + 1)?));
            }
        }
        _ => return Err(YAML),
    };
    Ok(Node {
        kind,
        start,
        end: end.index(),
    })
}

/// The one document of `text`.
fn document(text: &str) -> Result<Node> {
    let mut options = Options::default();
    options.set_emit_comments(false);
    let mut events =
        Parser::new_from_str_with_options(text, options).map(|event| event.map_err(|_| YAML));
    let opened = (next(&mut events)?.0, next(&mut events)?.0);
    require(
        matches!(opened, (Event::StreamStart, Event::DocumentStart(..))),
        YAML.0,
    )?;
    let root = node(next(&mut events)?, &mut events, 0)?;
    let closed = (next(&mut events)?.0, next(&mut events)?.0);
    require(
        closed == (Event::DocumentEnd, Event::StreamEnd) && events.next().is_none(),
        YAML.0,
    )?;
    Ok(root)
}

/// The feature at `route` in the workflow `text`, as the module says.
fn feature(text: &str, route: &[Step]) -> Result<String> {
    let root = document(text)?;
    let (mut found, mut start) = (&root, root.start);
    for step in route {
        (found, start) = match (step, &found.kind) {
            (Step::Key(name), Kind::Map(entries)) => {
                let named = |(key, _): &&(Node, Node)| match &key.kind {
                    Kind::Scalar(value) => value == name,
                    _ => false,
                };
                let mut matches = entries.iter().filter(named);
                let (Some((key, value)), None) = (matches.next(), matches.next()) else {
                    return Err(ROUTE);
                };
                (value, key.start)
            }
            (Step::Index(index), Kind::List(items)) => {
                let item = items.get(*index).ok_or(ROUTE)?;
                (item, item.start)
            }
            _ => return Err(ROUTE),
        };
    }
    match &found.kind {
        Kind::Scalar(value) => Ok(value.clone()),
        Kind::List(_) => Err(FEATURE),
        Kind::Map(_) => {
            let source: String = text.chars().take(found.end).skip(start).collect();
            Ok(source.trim_end_matches(space).to_owned())
        }
    }
}

/// Refuses `review` unless the file it names, below `root`, still holds at
/// the review's route the feature its fingerprint names. The file is read
/// whole, and is at most [`MAX_DOCUMENT`] bytes of UTF-8.
fn held(root: &Path, review: &Exception) -> Result<()> {
    let (name, steps) = review.scope.split_once('#').ok_or(PLACE)?;
    let (_, reviewed) = review.fingerprint.split_once(':').ok_or(PLACE)?;
    require(safe_name(name), PLACE.0)?;
    let content = files::bounded(&root.join(name), MAX_DOCUMENT, false)?;
    let text = std::str::from_utf8(&content).map_err(|_| YAML)?;
    let feature = feature(text, &route(steps)?)?;
    require(digest(feature.as_bytes()) == reviewed, CHANGED.0)
}

/// The zizmor reviews of the policy below `root`, each judged by [`held`].
fn judged(root: &Path) -> Vec<(Exception, Result<()>)> {
    // Read as of the first day: whether a review has expired is the gate's
    // question on the day it runs.
    policy::read(&root.join("security/exceptions.json"), Date::MIN, false)
        .expect("the policy")
        .into_iter()
        .filter(|review| review.scanner == "zizmor")
        .map(|review| {
            let verdict = held(root, &review);
            (review, verdict)
        })
        .collect()
}

/// The feature of `text` at the route `place`.
fn at(text: &str, place: &str) -> Result<String> {
    feature(text, &route(place)?)
}

#[test]
fn every_committed_zizmor_review_still_names_its_feature() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let reviews = judged(&root);
    assert!(!reviews.is_empty());
    for (review, verdict) in reviews {
        assert_eq!(verdict, Ok(()), "{}", review.scope);
    }
}

#[test]
fn a_route_is_keys_and_indexes() {
    let key = |name: &str| Step::Key(name.to_owned());
    assert_eq!(
        route("jobs/rust-lint/steps/5/uses"),
        Ok(vec![
            key("jobs"),
            key("rust-lint"),
            key("steps"),
            Step::Index(5),
            key("uses"),
        ])
    );
    assert_eq!(route("007"), Ok(vec![Step::Index(7)]));
    assert_eq!(route(""), Ok(vec![key("")]));
    assert_eq!(
        route("-1/1e3/\u{663}"),
        Ok(vec![key("-1"), key("1e3"), key("\u{663}")])
    );
    assert_eq!(route("0/99999999999999999999999999"), Err(ROUTE));
}

#[test]
fn a_scalar_feature_is_its_value_and_a_mapping_feature_its_source() {
    let trigger =
        "on:\n  workflow_run:\n    workflows: [CI]\n    types: [completed]\n    branches: [main]";
    let text = format!("name: Cache retention\n\n{trigger}\n\npermissions:\n  contents: read\n");
    assert_eq!(at(&text, "on"), Ok(trigger.to_owned()));
    assert_eq!(
        at(&text.replace("[main]", "[other]"), "on"),
        Ok(trigger.replace("[main]", "[other]"))
    );
    // A comment before the next key is inside the feature, and the last
    // mapping of a file ends with the file.
    let noted = text.replace("\n\npermissions", "\n\n# Read only.\npermissions");
    assert_eq!(at(&noted, "on"), Ok(format!("{trigger}\n\n# Read only.")));
    let last = "permissions:\n  contents: read";
    assert_eq!(at(&text, "permissions"), Ok(last.to_owned()));
    // Places are counted in characters, and a scalar is read as YAML reads it.
    let action = format!("owner/action@{}", "a".repeat(40));
    let steps = format!(
        "steps:\n  - uses: {action} # pinned\n  - name: \u{e9}\n    with: {{a: 1}} #\n  - run:\n"
    );
    assert_eq!(at(&steps, "steps/0/uses"), Ok(action));
    assert_eq!(at(&steps, "steps/1/with"), Ok("with: {a: 1}".to_owned()));
    let whole = "name: \u{e9}\n    with: {a: 1} #";
    assert_eq!(at(&steps, "steps/1"), Ok(whole.to_owned()));
    assert_eq!(at(&steps, "steps/2/run"), Ok(String::new()));
    assert_eq!(at("uses: ~\n", "uses"), Ok("~".to_owned()));
    assert_eq!(
        at("uses: 'it''s' # quoted\n", "uses"),
        Ok("it's".to_owned())
    );
}

#[test]
fn a_route_that_finds_no_single_feature_is_refused() {
    let twice = "jobs:\n  a:\n    steps:\n      - uses: one\n      - run: two\n  a:\n    uses: w\n";
    assert_eq!(at(twice, "jobs/a/steps/0/uses"), Err(ROUTE));
    assert_eq!(at(twice, "jobs/a/uses"), Err(ROUTE));
    assert_eq!(at(twice, "jobs/b/uses"), Err(ROUTE));
    let text = twice.replace("  a:\n    uses: w\n", "  b: [x]\n");
    assert_eq!(at(&text, "jobs/a/steps/0/uses"), Ok("one".to_owned()));
    for nowhere in [
        "jobs/a/steps/2/uses",
        "jobs/a/steps/1/uses",
        "jobs/a/steps/uses",
        "jobs/0",
        "jobs/a/steps/0/uses/more",
        "jobs/a/steps/0/99999999999999999999999999",
    ] {
        assert_eq!(at(&text, nowhere), Err(ROUTE), "{nowhere}");
    }
    assert_eq!(at(&text, "jobs/a/steps"), Err(FEATURE));
    assert_eq!(at(&text, "jobs/b"), Err(FEATURE));
    let nested = |depth: usize| format!("a: {}1{}\n", "[".repeat(depth), "]".repeat(depth));
    assert_eq!(at(&nested(MAX_DEPTH - 1), "a"), Err(FEATURE));
    for unread in [
        "",
        "a: 1\n---\nb: 2\n",
        "a: &one 1\n",
        "a: &one [1]\n",
        "a: &one {b: 1}\n",
        "a: 1\nb: &one 1\nc: *one\n",
        "a: !tagged 1\n",
        "a: !tagged [1]\n",
        "a: !tagged {b: 1}\n",
        "a: 1\n? [b, c]\n: 2\n",
        "a: [1\n",
        &nested(MAX_DEPTH),
    ] {
        assert_eq!(at(unread, "a"), Err(YAML), "{unread:?}");
    }
}

#[test]
fn a_review_of_a_moved_or_changed_feature_is_refused() {
    const WORKFLOW: &str = ".github/workflows/build.yml";
    let scratch = Scratch::new("workflow-reviews");
    let workflow = "on:\n  push:\n    branches: [main]\n\njobs:\n  build:\n    steps:\n      \
        - run: make\n      - uses: owner/action@v1 # reviewed\n";
    let review = |scanner: &str, fingerprint: &str, scope: &str| {
        serde_json::json!({
            "scanner": scanner, "fingerprint": fingerprint, "scope": scope, "owner": "owner",
            "rationale": "reviewed", "reachability": "none", "expires": "2099-01-01",
            "review": "https://example.com/review",
        })
    };
    let verdicts = |workflow: &str, reviews: &[serde_json::Value]| {
        let policy = serde_json::json!({"schemaVersion": 1, "exceptions": reviews});
        for (name, content) in [
            (WORKFLOW, workflow.to_owned()),
            ("security/exceptions.json", policy.to_string()),
        ] {
            let path = scratch.0.join(name);
            std::fs::create_dir_all(path.parent().expect("a directory")).expect("a directory");
            std::fs::write(path, content).expect("a file");
        }
        let verdicts = judged(&scratch.0).into_iter().map(|(_, verdict)| verdict);
        verdicts.collect::<Vec<_>>()
    };
    let action = format!("unpinned-uses:{}", digest(b"owner/action@v1"));
    let trigger = digest(b"on:\n  push:\n    branches: [main]");
    let trigger = format!("dangerous-triggers:{trigger}");
    let used = format!("{WORKFLOW}#jobs/build/steps/1/uses");
    let reviews = [
        review("zizmor", &action, &used),
        review("zizmor", &trigger, &format!("{WORKFLOW}#on")),
        review("cargo-audit", "RUSTSEC-2024-0001", "instant@0.1.13"),
    ];
    // Only the zizmor reviews are judged, in the order of the policy.
    assert_eq!(verdicts(workflow, &reviews), [Ok(()), Ok(())]);
    let moved = workflow.replace(
        "- run: make\n",
        "- run: make\n      - uses: owner/first@v1\n",
    );
    assert_eq!(verdicts(&moved, &reviews), [Err(CHANGED), Ok(())]);
    let moved = workflow.replace("- run: make\n", "- run: make\n      - run: test\n");
    assert_eq!(verdicts(&moved, &reviews), [Err(ROUTE), Ok(())]);
    let changed = workflow.replace("action@v1", "action@v2");
    assert_eq!(verdicts(&changed, &reviews), [Err(CHANGED), Ok(())]);
    let changed = workflow.replace("[main]", "[mail]");
    assert_eq!(verdicts(&changed, &reviews), [Ok(()), Err(CHANGED)]);
    let renamed = workflow.replace("  build:", "  tests:");
    assert_eq!(verdicts(&renamed, &reviews), [Err(ROUTE), Ok(())]);
    let other = format!("unpinned-uses:{}", digest(b"owner/action@v2"));
    let elsewhere = [
        review("zizmor", &other, &used),
        review("zizmor", &action, &format!("{WORKFLOW}#jobs/build/steps/1")),
        review("zizmor", &action, &format!("{WORKFLOW}#jobs/build/steps")),
        review("zizmor", &action, ".github/workflows/absent.yml#on"),
        review("zizmor", &action, "../build.yml#on"),
        review("zizmor", &action, WORKFLOW),
        review("zizmor", "unpinned-uses", &used),
        review("zizmor", &format!("zizmor:{action}"), &used),
    ];
    assert_eq!(
        verdicts(workflow, &elsewhere),
        [
            Err(CHANGED),
            Err(CHANGED),
            Err(FEATURE),
            Err(Fault("tool_entry_missing")),
            Err(PLACE),
            Err(PLACE),
            Err(PLACE),
            Err(CHANGED),
        ]
    );
    let bound = usize::try_from(MAX_DOCUMENT).expect("a bound");
    let long = format!("{workflow}#{}\n", "x".repeat(bound - workflow.len() - 2));
    assert_eq!(verdicts(&long, &reviews), [Ok(()), Ok(())]);
    let size = Err(Fault("tool_file_size"));
    assert_eq!(verdicts(&format!("{long}\n"), &reviews), [size, size]);
}
