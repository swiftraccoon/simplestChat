//! Replays of `recorded_cache.json`: what the reference in Python made of
//! the scanner's pin, of the trust of a run, of generations, of the site's
//! descriptors and the update's sources, of fetched descriptors, of checked
//! copies, and of whole restores and saves of the cache.

use super::{
    LATEST, Limits, copy_checked, descriptor_key, generation, key, key_from_status, pin, prefix,
    restore, save, trusted,
};
use crate::{
    document::Document,
    image::{
        notices::tests::built,
        policy::tests::{There, tallied},
        secrets::tests::{here, text, there},
    },
    scratch::{Scratch, base64},
    tools::transport::{Answer, Transport},
};
use std::{
    io::Read,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::Path,
    time::Duration,
};

const RECORDING: &[u8] = include_bytes!("recorded_cache.json");

fn recording() -> Document {
    Document::read(RECORDING).expect("a recording")
}

fn cases(name: &str) -> Vec<Document> {
    let held = recording()
        .get(name)
        .and_then(Document::items)
        .map(<[Document]>::to_vec);
    held.unwrap_or_else(|| panic!("no cases {name}"))
}

/// The pin that the recorded cases were made under.
fn recorded_pin() -> String {
    text(&recording(), "pin").to_owned()
}

/// The bounds of a case: the reviewed ones, or those it was recorded with.
fn limited(case: &Document) -> Limits {
    let mut limits = Limits::REVIEWED;
    let given = case
        .get("limits")
        .and_then(Document::fields)
        .unwrap_or_default();
    for (name, value) in given {
        let value = u64::try_from(value.whole().expect("a bound")).expect("a bound");
        match name.as_str() {
            "vulnerability.db" => limits.database = value,
            "import.json" => limits.import = value,
            other => panic!("no bound {other}"),
        }
    }
    limits
}

/// What a tree holds after a case, as the recorder wrote it: each entry's
/// kind, bytes or target, and the mode of those that the code itself set.
fn walked(root: &Path) -> Document {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).expect("a directory") {
            let path = entry.expect("an entry").path();
            let name = path
                .strip_prefix(root)
                .expect("below the root")
                .to_str()
                .expect("text")
                .to_owned();
            let kind = path.symlink_metadata().expect("an entry");
            let mode = kind.permissions().mode() & 0o7777;
            let shown = if kind.file_type().is_symlink() {
                let target = std::fs::read_link(&path).expect("a link");
                Document::record([("link", target.to_str().expect("text").into())])
            } else if kind.file_type().is_fifo() {
                Document::record([("pipe", true.into())])
            } else if kind.is_dir() {
                pending.push(path.clone());
                Document::record([("directory", true.into()), ("mode", u64::from(mode).into())])
            } else {
                let bytes = std::fs::read(&path).expect("bytes");
                Document::record([
                    ("bytes", crate::digest(&bytes).into()),
                    ("mode", u64::from(mode).into()),
                ])
            };
            found.push((name, shown));
        }
    }
    found.sort_by(|left, right| left.0.cmp(&right.0));
    Document::Record(found)
}

/// A recorded tree as `walked` shows one: the files' bytes by their digest,
/// and no mode that the umask of the recording chose.
fn expected(tree: &Document) -> Document {
    let mut shown = Vec::new();
    for (name, entry) in tree.fields().expect("a tree") {
        let kept = |mode: Option<i128>| mode.filter(|mode| matches!(mode, 0o600 | 0o700));
        let entry = if let Some(target) = entry.get("link") {
            Document::record([("link", target.clone())])
        } else if entry.get("pipe").is_some() {
            Document::record([("pipe", true.into())])
        } else if entry.get("directory").is_some() {
            let mode = entry.get("mode").and_then(Document::whole);
            Document::record([
                ("directory", true.into()),
                (
                    "mode",
                    kept(mode).map_or(Document::Null, |mode| {
                        u64::try_from(mode).expect("a mode").into()
                    }),
                ),
            ])
        } else {
            let packed = base64(text(entry, "file"));
            let mut bytes = Vec::new();
            let read = flate2::read::ZlibDecoder::new(&packed[..]).read_to_end(&mut bytes);
            read.expect("deflated bytes");
            let mode = entry.get("mode").and_then(Document::whole);
            Document::record([
                ("bytes", crate::digest(&bytes).into()),
                (
                    "mode",
                    kept(mode).map_or(Document::Null, |mode| {
                        u64::try_from(mode).expect("a mode").into()
                    }),
                ),
            ])
        };
        shown.push((name.clone(), entry));
    }
    Document::Record(shown)
}

/// What a case came to here, and the tree it left: a case that comes to
/// what it came to there must also leave the tree that it left there.
fn left(came: String, root: &Path, case: &Document) -> (String, String, String) {
    let there = there(case);
    let after = expected(case.get("after").expect("a tree"));
    let came = if came == there && settled(walked(root)) != after {
        format!("{came} with another tree")
    } else {
        came
    };
    (text(case, "name").to_owned(), came, there)
}

/// A walked tree with only the modes that the code itself set.
fn settled(tree: Document) -> Document {
    let Document::Record(fields) = tree else {
        panic!("a tree")
    };
    let fields = fields.into_iter().map(|(name, mut entry)| {
        let mode = entry.get("mode").and_then(Document::whole);
        if mode.is_some_and(|mode| !matches!(mode, 0o600 | 0o700)) {
            entry.put("mode", Document::Null);
        }
        (name, entry)
    });
    Document::Record(fields.collect())
}

fn pins() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("pins") {
        let came = Ok(Document::from(pin(case.get("grype").expect("a record"))));
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn prefixes() -> Vec<(String, String, String)> {
    let pinned = recorded_pin();
    let mut all = Vec::new();
    for case in cases("prefixes") {
        let (reference, event) = (
            case.get("reference").and_then(Document::text),
            case.get("event").and_then(Document::text),
        );
        let came = Ok(Document::from(prefix(trusted(reference, event), &pinned)));
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn trusted_prefix() -> String {
    prefix(true, &recorded_pin())
}

fn generations() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("generations") {
        let checksum = case
            .get("checksum")
            .filter(|checksum| **checksum != Document::Null);
        let came = generation(checksum, &trusted_prefix()).map(Document::from);
        all.push((text(&case, "name").to_owned(), here(came), there(&case)));
    }
    all
}

fn descriptors() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("descriptors") {
        let came = descriptor_key(
            case.get("descriptor").expect("a descriptor"),
            &trusted_prefix(),
        );
        all.push((
            text(&case, "name").to_owned(),
            here(came.map(Document::from)),
            there(&case),
        ));
    }
    all
}

fn sources() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("sources") {
        let came = key_from_status(case.get("status").expect("a status"), &trusted_prefix());
        all.push((
            text(&case, "name").to_owned(),
            here(came.map(Document::from)),
            there(&case),
        ));
    }
    all
}

/// The site as the recording answered: one response, from the address it
/// ended at, whatever its length, so that the bound of the descriptor is
/// held by the module and not only by the transport.
struct Site(Option<(Vec<u8>, String)>, Vec<(String, u64, Duration)>);

impl Transport for Site {
    fn now(&mut self) -> Duration {
        Duration::ZERO
    }

    fn pause(&mut self, _: Duration) {}

    fn fetch(&mut self, address: &str, limit: u64, within: Duration) -> Answer {
        self.1.push((address.to_owned(), limit, within));
        let (bytes, address) = self.0.take().expect("one answer");
        Answer::Body { bytes, address }
    }
}

fn fetches() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("fetches") {
        let mut site = Site(
            Some((base64(text(&case, "body")), text(&case, "url").to_owned())),
            Vec::new(),
        );
        let came = key(&mut site, &trusted_prefix(), &Limits::REVIEWED);
        assert_eq!(
            site.1,
            [(
                LATEST.to_owned(),
                Limits::REVIEWED.descriptor,
                Duration::from_secs(10)
            )]
        );
        all.push((
            text(&case, "name").to_owned(),
            here(came.map(Document::from)),
            there(&case),
        ));
    }
    all
}

fn copies() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("copies") {
        let scratch = Scratch::new("cache");
        let root = built(&case, &scratch.0);
        let limit = u64::try_from(
            case.get("limit")
                .and_then(Document::whole)
                .expect("a bound"),
        )
        .expect("a bound");
        let came = copy_checked(&root.join("source"), &root.join("copy"), limit);
        all.push(left(here(came), &root, &case));
    }
    all
}

fn restores() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("restores") {
        let scratch = Scratch::new("cache");
        let root = built(&case, &scratch.0);
        let relative = case.get("relative") == Some(&Document::Bool(true));
        let directory = if relative {
            Path::new("cache").to_path_buf()
        } else {
            root.join("cache")
        };
        let came = restore(
            &directory,
            &root.join("output"),
            None,
            &recorded_pin(),
            &limited(&case),
        );
        all.push(left(here(came.map(Document::from)), &root, &case));
    }
    all
}

fn saves() -> Vec<(String, String, String)> {
    let mut all = Vec::new();
    for case in cases("saves") {
        let scratch = Scratch::new("cache");
        let root = built(&case, &scratch.0);
        let relative = case.get("relative") == Some(&Document::Bool(true));
        let directory = if relative {
            Path::new("cache").to_path_buf()
        } else {
            root.join("cache")
        };
        let came = save(
            &directory,
            &root.join("database"),
            &recorded_pin(),
            &Limits::REVIEWED,
        );
        all.push(left(here(came.map(|()| Document::Null)), &root, &case));
    }
    all
}

/// A site that answers the one way it is told to.
struct Answering(Option<Answer>);

impl Transport for Answering {
    fn now(&mut self) -> Duration {
        Duration::ZERO
    }

    fn pause(&mut self, _: Duration) {}

    fn fetch(&mut self, _: &str, _: u64, _: Duration) -> Answer {
        self.0.take().expect("one answer")
    }
}

#[test]
fn a_descriptor_that_is_too_long_or_does_not_come_is_refused() {
    let refused = |answer: Answer| {
        let came = key(
            &mut Answering(Some(answer)),
            &trusted_prefix(),
            &Limits::REVIEWED,
        );
        came.expect_err("a refusal").0
    };
    assert_eq!(refused(Answer::Oversized), "image_database_generation_size");
    assert_eq!(
        refused(Answer::Status(404)),
        "image_database_generation_fetch"
    );
    assert_eq!(refused(Answer::Late), "image_database_generation_fetch");
    assert_eq!(refused(Answer::Failed), "image_database_generation_fetch");
}

/// A descriptor that the reference took because its pattern's digits were
/// those of any script; a schema's numbers are digits of ASCII here.
const STRICTER_DESCRIPTORS: [(&str, &str); 1] = [(
    "a descriptor of a schema of other digits",
    "image_database_generation_descriptor",
)];

/// Sources that the reference failed on in its interpreter's words: a
/// query field without a sign, or an empty one.
const UNREAD_SOURCES: [(&str, &str); 3] = [
    (
        "a source of a field without a value",
        "image_database_generation_source",
    ),
    (
        "a source of a lone field without a sign",
        "image_database_generation_source",
    ),
    (
        "a source of an empty part",
        "image_database_generation_source",
    ),
];

/// Sources that the reference's URL library took: it dropped tabs and line
/// ends, took a scheme in any case and an empty fragment. The address is
/// read here as it is written, as the database's status is.
const STRICTER_SOURCES: [(&str, &str); 7] = [
    ("a source of a line end", "image_database_generation_source"),
    (
        "a source of a line end in its archive",
        "image_database_generation_source",
    ),
    (
        "a source of a return in its archive",
        "image_database_generation_source",
    ),
    (
        "a source of a scheme in capitals",
        "image_database_generation_source",
    ),
    ("a source of a tab", "image_database_generation_source"),
    (
        "a source of a tab in its archive",
        "image_database_generation_source",
    ),
    (
        "a source of an empty fragment",
        "image_database_generation_source",
    ),
];

/// Fetched descriptors that the reference failed on in its interpreter's
/// words.
const UNREAD_FETCHES: [(&str, &str); 2] = [
    (
        "a descriptor that is a list",
        "image_database_generation_source",
    ),
    (
        "a descriptor that is no JSON",
        "image_database_generation_source",
    ),
];

/// Copies that the reference failed on in its interpreter's words: a link,
/// a directory, nothing, or a copy that is already there.
const UNREAD_COPIES: [(&str, &str); 4] = [
    (
        "a copy that is already there",
        "image_database_cache_unread",
    ),
    ("a directory", "image_database_cache_unread"),
    ("a link to a file", "image_database_cache_unread"),
    ("no file", "image_database_cache_unread"),
];

/// Restores that the reference failed on in its interpreter's words.
const UNREAD_RESTORES: [(&str, &str); 8] = [
    (
        "a cache without its database",
        "image_database_cache_unread",
    ),
    ("a cache without its receipt", "image_database_cache_unread"),
    ("a database that is a link", "image_database_cache_unread"),
    ("a receipt that is a link", "image_database_cache_unread"),
    ("a receipt that is a list", "image_database_cache_unread"),
    ("a receipt that is no JSON", "image_database_cache_unread"),
    (
        "a receipt whose files are a list",
        "image_database_cache_unread",
    ),
    (
        "an output that already has a cache",
        "image_database_cache_unread",
    ),
];

/// A receipt that the reference took because its interpreter holds 1 equal
/// to true; its schema is held here to the number.
const SAME_KIND: [(&str, &str); 1] = [(
    "a receipt whose schema is true",
    "image_database_cache_receipt",
)];

/// Saves that the reference failed on in its interpreter's words.
const UNREAD_SAVES: [(&str, &str); 3] = [
    ("a cache that is a file", "image_database_cache_unread"),
    (
        "a database whose file is a link",
        "image_database_cache_unread",
    ),
    (
        "a database without its import record",
        "image_database_cache_unread",
    ),
];

#[test]
fn every_recorded_pin_is_the_same() {
    assert_eq!(tallied(pins(), &[]), [5, 0, 0, 0, 0]);
}

#[test]
fn every_recorded_prefix_is_the_same() {
    assert_eq!(tallied(prefixes(), &[]), [8, 0, 0, 0, 0]);
}

#[test]
fn every_recorded_generation_is_the_same() {
    assert_eq!(tallied(generations(), &[]), [1, 9, 0, 0, 0]);
}

#[test]
fn every_recorded_descriptor_is_read_alike() {
    let tables = [(&STRICTER_DESCRIPTORS[..], There::Held)];
    assert_eq!(tallied(descriptors(), &tables), [4, 12, 0, 1, 0]);
}

#[test]
fn every_recorded_source_is_read_alike() {
    let tables = [
        (&UNREAD_SOURCES[..], There::Raised),
        (&STRICTER_SOURCES[..], There::Held),
    ];
    assert_eq!(tallied(sources(), &tables), [6, 22, 3, 7, 0]);
}

#[test]
fn every_recorded_fetch_is_read_alike() {
    let tables = [(&UNREAD_FETCHES[..], There::Raised)];
    assert_eq!(tallied(fetches(), &tables), [2, 4, 2, 0, 0]);
}

#[test]
fn every_recorded_copy_is_made_alike() {
    let tables = [(&UNREAD_COPIES[..], There::Raised)];
    assert_eq!(tallied(copies(), &tables), [2, 4, 4, 0, 0]);
}

#[test]
fn every_recorded_restore_is_made_alike() {
    let tables = [
        (&UNREAD_RESTORES[..], There::Raised),
        (&SAME_KIND[..], There::Held),
    ];
    assert_eq!(tallied(restores(), &tables), [6, 16, 8, 1, 0]);
}

#[test]
fn every_recorded_save_is_made_alike() {
    let tables = [(&UNREAD_SAVES[..], There::Raised)];
    assert_eq!(tallied(saves(), &tables), [3, 4, 3, 0, 0]);
}

#[test]
fn the_bounds_are_the_reviewed_ones() {
    let recording = recording();
    let bounds = recording.get("limits").expect("bounds");
    let bound = |name: &str| bounds.get(name).and_then(Document::whole).expect("a bound");
    assert_eq!(
        bound("vulnerability.db"),
        i128::from(Limits::REVIEWED.database)
    );
    assert_eq!(bound("import.json"), i128::from(Limits::REVIEWED.import));
    assert_eq!(text(&recording, "latest"), LATEST);
}
