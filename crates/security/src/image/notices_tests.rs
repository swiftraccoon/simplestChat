//! Replays of `recorded_notices.json`: what the reference in Python made
//! of notice paths, of the identities of rpm packages, of their licence
//! declarations and of whole collections over image file systems.

use super::{Image, Limits, collect, license_identity, notice_path, package_identity};
use crate::{
    document::Document,
    scratch::{Scratch, base64},
};
use std::{io::Read, os::unix::fs::PermissionsExt, path::PathBuf};

const RECORDING: &[u8] = include_bytes!("recorded_notices.json");

fn cases(name: &str) -> Vec<Document> {
    let recording = Document::read(RECORDING).expect("a recording");
    let held = recording.get(name).and_then(Document::items);
    held.unwrap_or_else(|| panic!("no cases {name}")).to_vec()
}

fn text<'a>(case: &'a Document, name: &str) -> &'a str {
    let held = case.get(name).and_then(Document::text);
    held.unwrap_or_else(|| panic!("no text {name}"))
}

/// What a case came to there: what was held, the refusal, or the class of
/// what the reference's interpreter raised.
fn there(case: &Document) -> String {
    let result = case.get("result").expect("a result");
    if let Some(held) = result.get("held") {
        return format!("held {}", held.pretty());
    }
    if let Some(words) = result.get("refused").and_then(Document::text) {
        return format!("refused {words}");
    }
    format!("RAISED {}", text(result, "raised"))
}

fn here(came: crate::Result<Document>) -> String {
    match came {
        Ok(held) => format!("held {}", held.pretty()),
        Err(fault) => format!("refused {fault}"),
    }
}

fn paths() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("paths") {
        let value = match case.get("value").and_then(Document::text) {
            Some(value) => value.to_owned(),
            None => {
                let size = case
                    .get("long")
                    .and_then(Document::whole)
                    .expect("a length");
                let accented = case.get("accented") == Some(&Document::Bool(true));
                let letter = if accented { "\u{e9}" } else { "x" };
                let more = usize::try_from(size).expect("a length") - 20;
                format!("/usr/share/licenses/{}", letter.repeat(more))
            }
        };
        let came = notice_path(&value, &Limits::REVIEWED).map(Document::from);
        let shown: String = value.chars().take(60).collect();
        let name = format!("{shown:?} of {}", value.chars().count());
        all.push((name, here(came), there(&case)));
    }
    all
}

fn packages() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("packages") {
        let package = case.get("package").expect("a package");
        let came = package_identity(package, text(&case, "architecture"));
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn licenses() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("licenses") {
        let declared = case.get("licenses").expect("licences").clone();
        let package = Document::record([("licenses", declared)]);
        let mut limits = Limits::REVIEWED;
        if let Some(limit) = case.get("limit").and_then(Document::whole) {
            limits.license_bytes = usize::try_from(limit).expect("a bound");
        }
        let came = license_identity(&package, &limits);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

/// The bounds of a case: the reviewed ones but for those it names as the
/// reference's module names them.
fn limits(case: &Document) -> Limits {
    let mut limits = Limits::REVIEWED;
    for (name, value) in case
        .get("limits")
        .and_then(Document::fields)
        .expect("limits")
    {
        let value = value.whole().expect("a bound");
        let (wide, sized) = (u64::try_from(value).ok(), usize::try_from(value).ok());
        match name.as_str() {
            "MAX_PACKAGES" => limits.packages = sized.expect("a bound"),
            "MAX_METADATA_FILES" => limits.metadata_files = sized.expect("a bound"),
            "MAX_NOTICES" => limits.notices = sized.expect("a bound"),
            "MAX_NOTICE_BYTES" => limits.notice_bytes = wide.expect("a bound"),
            "MAX_TOTAL_BYTES" => limits.total_bytes = wide.expect("a bound"),
            "MAX_REPORT_BYTES" => limits.report_bytes = sized.expect("a bound"),
            other => panic!("no bound {other}"),
        }
    }
    limits
}

/// Build the file system of a case below `base` and answer the root that
/// the case hands to the collection.
pub(crate) fn built(case: &Document, base: &std::path::Path) -> PathBuf {
    let root = base.join("root");
    std::fs::create_dir(&root).expect("a root");
    for (path, entry) in case.get("tree").and_then(Document::fields).expect("a tree") {
        let target = root.join(path);
        std::fs::create_dir_all(target.parent().expect("a directory")).expect("a directory");
        if entry.get("directory").is_some() {
            std::fs::create_dir_all(&target).expect("a directory");
        } else if entry.get("pipe").is_some() {
            let made = std::process::Command::new("mkfifo").arg(&target).status();
            assert!(made.expect("mkfifo").success());
        } else if let Some(link) = entry.get("link").and_then(Document::text) {
            std::os::unix::fs::symlink(link, &target).expect("a link");
        } else {
            let packed = base64(text(entry, "file"));
            let mut data = Vec::new();
            let read = flate2::read::ZlibDecoder::new(&packed[..]).read_to_end(&mut data);
            read.expect("deflated bytes");
            std::fs::write(&target, data).expect("a file");
        }
    }
    // A recorded mode is given last, children before what holds them.
    for (path, entry) in case
        .get("tree")
        .and_then(Document::fields)
        .expect("a tree")
        .iter()
        .rev()
    {
        if let Some(mode) = entry.get("mode").and_then(Document::whole) {
            let mode = std::fs::Permissions::from_mode(u32::try_from(mode).expect("a mode"));
            std::fs::set_permissions(root.join(path), mode).expect("a mode");
        }
    }
    match text(case, "rootfs") {
        "root" => root,
        "link" => {
            std::os::unix::fs::symlink("root", base.join("alias")).expect("a link");
            base.join("alias")
        }
        "file" => {
            std::fs::write(base.join("plain"), b"").expect("a file");
            base.join("plain")
        }
        "absent" => base.join("nothing"),
        "relative" => PathBuf::from("root"),
        "dotted" => root.join("..").join("root"),
        other => panic!("no root {other}"),
    }
}

fn collections() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("collects") {
        let scratch = Scratch::new("notices");
        let rootfs = built(&case, &scratch.0);
        let arguments = case.get("arguments").expect("arguments");
        let image = Image {
            image_id: text(arguments, "image_id"),
            archive_sha256: text(arguments, "archive_sha256"),
            sbom_sha256: text(arguments, "sbom_sha256"),
            revision: text(arguments, "revision"),
            platform: text(arguments, "platform"),
        };
        let packages = case
            .get("packages")
            .and_then(Document::items)
            .expect("packages");
        let came = collect(&rootfs, packages, &image, &limits(&case));
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

/// Hold every case to the recording, but for those that `reworded` names
/// with the code they have here, where the reference failed in its
/// interpreter's words, and those that `stricter` names with the code that
/// refuses here what the reference held. Answers how many were held
/// alike, refused alike, reworded and refused only here.
fn tallied(
    all: Vec<(String, String, String)>,
    reworded: &[(&str, &str)],
    stricter: &[(&str, &str)],
) -> [usize; 4] {
    let mut tally = [0; 4];
    for (name, here, there) in all {
        let named = |table: &[(&str, &str)]| {
            let found = table.iter().find(|(other, _)| *other == name);
            found.map(|(_, code)| format!("refused {code}"))
        };
        if let Some(code) = named(reworded) {
            assert_eq!(here, code, "{name}");
            assert!(there.starts_with("RAISED "), "{name}");
            tally[2] += 1;
        } else if let Some(code) = named(stricter) {
            assert_eq!(here, code, "{name}");
            assert!(there.starts_with("held "), "{name}");
            tally[3] += 1;
        } else {
            assert_eq!(here, there, "{name}");
            tally[usize::from(here.starts_with("refused "))] += 1;
        }
    }
    tally
}

/// Packages that the reference failed on in its interpreter's words: a
/// missing field, a value of another kind, a query that is no list of
/// names and values.
const UNDESCRIBED: [(&str, &str); 15] = [
    ("a package without its name", "rpm_notice_package"),
    ("a package without its version", "rpm_notice_package"),
    ("a package without its purl", "rpm_notice_package"),
    ("a package without its metadata", "rpm_notice_package"),
    ("metadata without its name", "rpm_notice_identity"),
    ("metadata without its version", "rpm_notice_package"),
    ("metadata without its release", "rpm_notice_package"),
    ("metadata without its sourceRpm", "rpm_notice_package"),
    ("metadata without its epoch", "rpm_notice_package"),
    ("metadata without its architecture", "rpm_notice_package"),
    ("a name that is a number", "rpm_notice_package"),
    (
        "an address with a qualifier without its sign",
        "rpm_notice_purl",
    ),
    ("an address with an empty qualifier", "rpm_notice_purl"),
    (
        "an address with two signs between qualifiers",
        "rpm_notice_purl",
    ),
    ("an address that is a number", "rpm_notice_package"),
];

/// Addresses that the reference's URL library took after changing them:
/// it read the kind without regard to case, dropped white space before
/// the address and tabs and line ends anywhere in it, and took an empty
/// fragment for none. An address is held here as it is written.
const LOOSE: [(&str, &str); 5] = [
    ("an address with an empty fragment", "rpm_notice_purl"),
    ("an address with its kind in capitals", "rpm_notice_purl"),
    ("an address with a space before it", "rpm_notice_purl"),
    ("an address with a line end inside it", "rpm_notice_purl"),
    ("an address with a tab inside its query", "rpm_notice_purl"),
];

/// Licence declarations of another kind than the reference could read.
const UNDECLARED: [(&str, &str); 4] = [
    ("an expression that is a number", "rpm_notice_package"),
    ("an expression that is nothing", "rpm_notice_package"),
    ("a record that is text", "image_license_records"),
    ("records that are no list", "rpm_notice_package"),
];

/// Collections that the reference failed on in its interpreter's words:
/// a record of a file or of a package that lacks a field or has one of
/// another kind, a notice or a root that is not there.
const UNCOLLECTED: [(&str, &str); 17] = [
    (
        "a notice that is a link to nothing",
        "rpm_notice_file_missing",
    ),
    ("a notice that is not there", "rpm_notice_file_missing"),
    (
        "a notice recorded with a mode in text",
        "rpm_notice_file_metadata",
    ),
    (
        "a notice recorded with a true mode",
        "rpm_notice_file_metadata",
    ),
    (
        "a notice recorded with a size in text",
        "rpm_notice_file_metadata",
    ),
    (
        "a notice recorded with a path that is a number",
        "rpm_notice_file_metadata",
    ),
    (
        "a notice recorded with a digest that is text",
        "rpm_notice_file_metadata",
    ),
    (
        "a notice recorded without its path",
        "rpm_notice_file_metadata",
    ),
    (
        "a notice recorded without its mode",
        "rpm_notice_file_metadata",
    ),
    (
        "a notice recorded without its size",
        "rpm_notice_file_metadata",
    ),
    (
        "a notice recorded without its digest",
        "rpm_notice_file_metadata",
    ),
    (
        "a record of a file that is no record",
        "rpm_notice_file_metadata",
    ),
    ("files that are no list", "rpm_notice_package"),
    ("a static input without an address", "rpm_notice_package"),
    ("a package that is no record", "rpm_notice_package"),
    ("a package without licences", "rpm_notice_package"),
    ("a root that is not there", "rpm_notice_rootfs"),
];

#[test]
fn an_escape_is_a_percent_sign_and_two_hexadecimal_digits() {
    // The reference refused an address with any other percent sign.
    let table = [
        ("plain", Some("plain")),
        ("%41%6a%6A", Some("Ajj")),
        ("a%C3%A9", Some("a\u{e9}")),
        ("caf\u{e9}%20", Some("caf\u{e9} ")),
        ("%", None),
        ("%4", None),
        ("%zz", None),
        ("%+4", None),
        ("%4 ", None),
        ("a%", None),
        ("%ff", None),
    ];
    for (text, said) in table {
        assert_eq!(super::unquoted(text).as_deref(), said, "{text}");
    }
}

#[test]
fn every_recorded_path_is_judged_alike() {
    assert_eq!(tallied(paths(), &[], &[]), [48, 16, 0, 0]);
}

#[test]
fn every_recorded_package_is_identified_alike() {
    assert_eq!(tallied(packages(), &UNDESCRIBED, &LOOSE), [22, 52, 15, 5]);
}

#[test]
fn every_recorded_licence_declaration_is_identified_alike() {
    assert_eq!(tallied(licenses(), &UNDECLARED, &[]), [11, 3, 4, 0]);
}

#[test]
fn every_recorded_collection_comes_to_the_same() {
    assert_eq!(tallied(collections(), &UNCOLLECTED, &[]), [33, 62, 17, 0]);
}
