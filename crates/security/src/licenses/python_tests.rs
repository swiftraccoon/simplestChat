//! Replays of what the reference made of Python artifacts.
//!
//! `recorded_python.json` holds the normal forms and file-name readings of
//! the packaging library that `build/security_dependency_licenses.py`
//! uses, and what the module itself answered for core metadata and for
//! whole pages of the package index served by a scripted `curl`. Where the
//! reference failed with an exception of its interpreter or its library,
//! `named` gives the code here; the `STRICTER` lists name what it admitted
//! and is refused here.

use super::{
    Dependency,
    artifact::{sdist, wheel},
    metadata::{REVIEWED, Reviewed, metadata_license},
    python::python_licenses,
    registry::{Bounds, Evidence, Registry},
    registry_tests::session,
    tests::{content, list, text},
    version::normal,
};
use crate::{policy::space, scratch::Scratch};
use serde_json::{Value, json};
use std::{collections::BTreeSet, time::Duration};

const RECORDED: &str = include_str!("recorded_python.json");

/// A wheel's name and the tag of its interpreter are ASCII.
const STRICTER_NAMES: [&str; 3] = [
    "ex\u{e4}mple-1.0-py3-none-any.whl",
    "\u{212a}elvin-1.0-py3-none-any.whl",
    "example-1.0-p\u{ff}3-none-any.whl",
];

/// The fields are fields alone, without the envelope line of a mailbox or
/// a content type, and a value that is read is spelled out.
const STRICTER_METADATA: [&str; 8] = [
    "an envelope line first",
    "an envelope line last",
    "an encoded word as the expression",
    "an encoded word as the name",
    "an encoded word as the version",
    "the start of an encoded word in the expression",
    "a content type of plain text",
    "a content type of parts that are there",
];

/// An address is written as it is asked for.
const STRICTER_PAGES: [&str; 2] = [
    "a locked file with a scheme in capitals",
    "a locked file with a space in its address",
];

fn recorded() -> Value {
    serde_json::from_str(RECORDED).expect("the recording is JSON")
}

#[test]
fn every_recorded_version_has_the_same_normal_form() {
    let recording = recorded();
    let (mut spelled, mut refused) = (0, 0);
    for case in list(&recording["normal"]) {
        let version = text(&case["version"]);
        // The library reads a version between any space; so do its callers here.
        let read = normal(version.trim_matches(space));
        match case["result"].get("normal") {
            Some(reference) => {
                assert_eq!(read.as_deref(), Some(text(reference)), "{version:?}");
                spelled += 1;
            }
            None => {
                assert_eq!(
                    text(&case["result"]["code"]),
                    "InvalidVersion",
                    "{version:?}"
                );
                assert_eq!(read, None, "{version:?}");
                refused += 1;
            }
        }
    }
    assert_eq!((spelled, refused), (89, 41));
}

#[test]
fn every_recorded_file_name_is_read_alike() {
    let recording = recorded();
    let (mut read, mut refused, mut stricter) = (0, 0, 0);
    for case in list(&recording["names"]) {
        let filename = text(&case["filename"]);
        let named = if filename.ends_with(".whl") {
            wheel(filename)
        } else {
            sdist(filename)
        };
        let reference = &case["result"];
        if STRICTER_NAMES.contains(&filename) {
            assert!(reference.get("code").is_none(), "{filename:?}");
            assert_eq!(named, None, "{filename:?}");
            stricter += 1;
        } else if let Some(code) = reference.get("code") {
            let invalid = ["InvalidWheelFilename", "InvalidSdistFilename"];
            assert!(invalid.contains(&text(code)), "{filename:?}");
            assert_eq!(named, None, "{filename:?}");
            refused += 1;
        } else {
            let (name, version) = named.expect("a name that is read");
            assert_eq!(
                json!({"name": name, "version": version}),
                *reference,
                "{filename:?}"
            );
            read += 1;
        }
    }
    assert_eq!((read, refused, stricter), (42, 44, 3));
}

fn dependency(case: &Value) -> Dependency {
    let hashes = list(&case["hashes"])
        .iter()
        .map(|hash| text(hash).to_owned());
    Dependency {
        ecosystem: "pypi",
        manifest: "requirements.txt".to_owned(),
        name: text(&case["package"]).to_owned(),
        version: text(&case["version"]).to_owned(),
        identity: "identity".to_owned(),
        hashes: hashes.collect(),
        license: String::new(),
        source: String::new(),
    }
}

/// One review as a recording holds it.
fn review(entry: &Value) -> Reviewed<'_> {
    let [name, version, metadata, wheel, license] =
        [0, 1, 2, 3, 4].map(|index| text(&entry[index]));
    Reviewed {
        name,
        version,
        metadata,
        wheel,
        license,
    }
}

/// The reviews a recording holds as a list.
fn reviews(recorded: &Value) -> Vec<Reviewed<'_>> {
    list(recorded).iter().map(review).collect()
}

#[test]
fn every_recorded_metadata_is_read_alike() {
    let recording = recorded();
    let (mut read, mut refused, mut stricter) = (0, 0, 0);
    for case in list(&recording["metadata"]) {
        let name = text(&case["name"]);
        let limit = case["limit"].as_u64().unwrap_or(Bounds::REVIEWED.metadata);
        let license = metadata_license(
            &content(&case["content"]),
            &dependency(case),
            &reviews(&case["reviews"]),
            limit,
        );
        let failure = license.as_ref().err().map(|fault| fault.0);
        let reference = &case["result"];
        if STRICTER_METADATA.contains(&name) {
            assert!(reference.get("code").is_none(), "{name}");
            assert_eq!(failure, Some("dependency_python_metadata_format"), "{name}");
            stricter += 1;
        } else if let Some(code) = reference.get("code") {
            assert_eq!(failure, Some(text(code)), "{name}");
            refused += 1;
        } else {
            assert_eq!(
                license.ok().as_deref(),
                Some(text(&reference["license"])),
                "{name}"
            );
            read += 1;
        }
    }
    assert_eq!((read, refused, stricter), (41, 45, 8));
}

/// The code here for an exception of the reference: a page or one of its
/// files in another shape is no page of the index, a locked file without an
/// address has no origin, a wheel without the hash of its metadata has
/// none, and a file name that is none is not that of the locked version.
fn named<'a>(code: &'a str, name: &str) -> &'a str {
    match code {
        "InvalidWheelFilename" => "dependency_python_wheel_identity",
        "InvalidSdistFilename" => "dependency_python_sdist_identity",
        "KeyError" | "ValueError" | "JSONDecodeError" | "DuplicateJsonError" => {
            if name.starts_with("a locked file") {
                "dependency_python_wheel_origin"
            } else if name.starts_with("a wheel") {
                "dependency_python_metadata_hash"
            } else {
                "dependency_python_index_format"
            }
        }
        code => code,
    }
}

#[test]
fn every_recorded_page_of_the_index_is_judged_alike() {
    let recording = recorded();
    let reviewed = reviews(&recording["fixture"]);
    let scratch = Scratch::new("lp");
    let (mut held, mut refused, mut stricter) = (0, 0, 0);
    for (index, case) in list(&recording["python"]).iter().enumerate() {
        let name = text(&case["name"]);
        let session = session(
            &scratch.0.join(index.to_string()),
            list(&case["answers"]),
            false,
        );
        let files = case["bounds"]
            .get("MAX_INDEX_FILES")
            .and_then(Value::as_u64);
        let bounds = Bounds {
            files: files.map_or(Bounds::REVIEWED.files, |files| files as usize),
            ..Bounds::REVIEWED
        };
        let since = || Duration::ZERO;
        let mut registry = Registry::new(&session.curl, &since, bounds);
        let context = &session.context;
        let judged = python_licenses(context, &mut registry, &dependency(case), &reviewed);
        let judged = judged.map(|evidence| {
            let evidence: Vec<Value> = evidence.iter().map(Evidence::record).collect();
            json!({"evidence": evidence})
        });
        let failure = judged.as_ref().err().map(ToString::to_string);
        let reference = &case["result"];
        if STRICTER_PAGES.contains(&name) {
            assert!(reference.get("code").is_none(), "{name}");
            assert_eq!(
                failure.as_deref(),
                Some("dependency_python_wheel_origin"),
                "{name}"
            );
            stricter += 1;
            continue;
        }
        if let Some(code) = reference.get("code") {
            assert_eq!(failure.as_deref(), Some(named(text(code), name)), "{name}");
            refused += 1;
        } else {
            assert_eq!(judged.as_ref().ok(), Some(reference), "{name}");
            held += 1;
        }
        let asked = session.calls();
        let asked: Vec<&Value> = list(&asked).iter().collect();
        let last = |call: &&Value| {
            call["argv"]
                .as_array()
                .and_then(|argv| argv.last())
                .cloned()
        };
        let addresses: Vec<Value> = asked.iter().filter_map(last).collect();
        assert_eq!(json!(addresses), case["addresses"], "{name}");
        let sized = |call: &&Value| {
            let argv = list(&call["argv"]);
            let at = argv.iter().position(|word| word == "--max-filesize")?;
            argv.get(at + 1).cloned()
        };
        let limits: Vec<Value> = asked.iter().filter_map(sized).collect();
        assert_eq!(json!(limits), case["limits"], "{name}");
        assert_eq!(json!(registry.requests()), case["requests"], "{name}");
    }
    assert_eq!((held, refused, stricter), (21, 55, 2));
}

#[test]
fn the_reviewed_licences_and_bounds_are_the_recorded_ones() {
    let recording = recorded();
    let reviews: BTreeSet<Vec<&str>> = REVIEWED
        .iter()
        .map(|review| {
            vec![
                review.name,
                review.version,
                review.metadata,
                review.wheel,
                review.license,
            ]
        })
        .collect();
    let recorded: BTreeSet<Vec<&str>> = list(&recording["legacy"])
        .iter()
        .map(|entry| list(entry).iter().map(text).collect())
        .collect();
    assert_eq!(reviews, recorded);
    assert_eq!(reviews.len(), REVIEWED.len());
    assert_eq!(
        json!(Bounds::REVIEWED.files),
        recording["reviewed"]["MAX_INDEX_FILES"]
    );
}
