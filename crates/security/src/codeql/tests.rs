//! What the replays of the CodeQL steps share: the recordings' shape and
//! the tally of how each recorded case came out here. A case the reference
//! held must be held here alike; one it refused with a fixed code must be
//! refused with that code; one its interpreter failed in its own words is
//! refused here with the code that the replay's table names for it, and a
//! case it held that is refused here on purpose is listed as stricter.

use super::{Sourced, Stop, alike, lines, sources::Shown, words};
use crate::{document::Document, scratch::base64};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    fs,
    os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _, symlink},
    path::{Path, PathBuf},
};

/// The bytes a recorded file holds: text with the case's place written
/// in, base64, or one text repeated between a beginning and an end.
pub(crate) fn bytes(spec: &Document, place: &Path) -> Vec<u8> {
    if let Some(written) = spec.get("text").and_then(Document::text) {
        return written
            .replace("<place>", &place.to_string_lossy())
            .into_bytes();
    }
    if let Some(encoded) = spec.get("base64").and_then(Document::text) {
        return base64(encoded);
    }
    let times = usize::try_from(whole(spec, "times")).expect("a count");
    let part = |name: &str| spec.get(name).and_then(Document::text).unwrap_or_default();
    let mut written = part("first").to_owned();
    written.push_str(&part("repeat").repeat(times));
    written.push_str(part("then"));
    written.into_bytes()
}

/// Build the recorded tree `tree` below `root` as the recorder built it, in
/// the order of its names: a file, many empty files, a sparse file, a
/// second name for a file, a pipe, a link to a place below `root` and a
/// directory; an entry of nothing is absent. The modes and times it names
/// are given last, the deepest entries first.
pub(crate) fn planted(root: &Path, tree: &Document) {
    let mut entries: Vec<&(String, Document)> = tree.fields().expect("a tree").iter().collect();
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    entries.retain(|(_, spec)| *spec != Document::Null);
    for (name, spec) in &entries {
        let path = root.join(name);
        fs::create_dir_all(path.parent().expect("a parent")).expect("directories");
        let number = |field: &str| spec.get(field).and_then(Document::whole);
        if let Some(target) = spec.get("link").and_then(Document::text) {
            let target = target.replace("<place>", &root.to_string_lossy());
            symlink(target, &path).expect("a link");
        } else if spec.get("directory").is_some() {
            fs::create_dir_all(&path).expect("a directory");
        } else if let Some(count) = number("files") {
            fs::create_dir_all(&path).expect("a directory");
            for index in 0..count {
                let mut options = fs::OpenOptions::new();
                options.write(true).create(true).mode(0o600);
                options
                    .open(path.join(format!("f{index}")))
                    .expect("an empty file");
            }
        } else if let Some(size) = number("sparse") {
            let file = fs::File::create(&path).expect("a file");
            file.set_len(u64::try_from(size).expect("a size"))
                .expect("a length");
        } else if let Some(original) = spec.get("hardlink").and_then(Document::text) {
            fs::hard_link(root.join(original), &path).expect("a second name");
        } else if spec.get("fifo").is_some() {
            let made = std::process::Command::new("mkfifo").arg(&path).status();
            assert!(made.expect("mkfifo runs").success(), "a pipe");
        } else {
            fs::write(&path, bytes(spec, root)).expect("a file");
        }
    }
    entries.sort_by_key(|(name, _)| (std::cmp::Reverse(name.matches('/').count()), name.clone()));
    for (name, spec) in &entries {
        let mode = spec.get("mode").and_then(Document::whole);
        if let Some(mode) = mode.filter(|_| spec.get("link").is_none()) {
            let mode = fs::Permissions::from_mode(u32::try_from(mode).expect("a mode"));
            fs::set_permissions(root.join(name), mode).expect("a mode that is set");
        }
    }
    for (name, spec) in &entries {
        if let Some(time) = spec.get("mtime").and_then(Document::whole) {
            let time = rustix::fs::Timespec {
                tv_sec: i64::try_from(time).expect("a time"),
                tv_nsec: 0,
            };
            let times = rustix::fs::Timestamps {
                last_access: time,
                last_modification: time,
            };
            let flags = rustix::fs::AtFlags::SYMLINK_NOFOLLOW;
            rustix::fs::utimensat(rustix::fs::CWD, root.join(name), &times, flags).expect("a time");
        }
    }
}

/// Let every directory below `root` be entered again, so that the place
/// of a case that locked one can be removed.
pub(crate) fn unlocked(root: &Path) {
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let _ = fs::set_permissions(&directory, fs::Permissions::from_mode(0o700));
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                pending.push(entry.path());
            }
        }
    }
}

/// A refusal of a step that reads sources, as the tally takes it: a code
/// as it is, and the vendor verifier's words after `integrity: `, each with
/// the place `place` written as the recording writes it.
pub(crate) fn refusal(stop: &Stop, place: &Path) -> String {
    match stop {
        Stop::Code(code) => (*code).to_owned(),
        Stop::Problem(problem) => format!("integrity: {}", scrubbed(&problem.0, place)),
    }
}

/// `text` with the place `place` and every random staging name written as
/// the recording writes them.
pub(crate) fn scrubbed(text: &str, place: &Path) -> String {
    let text = text.replace(&*place.to_string_lossy(), "<place>");
    let marker = ".codeql-install-";
    let mut kept = String::new();
    let mut rest = text.as_str();
    while let Some(at) = rest.find(marker) {
        let after = &rest[at + marker.len()..];
        let random = after
            .bytes()
            .take(8)
            .take_while(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
            .count();
        kept.push_str(&rest[..at + marker.len()]);
        if random == 8 {
            kept.push_str("<random>");
            rest = &after[8..];
        } else {
            rest = after;
        }
    }
    kept.push_str(rest);
    kept
}

/// `data` in standard base64.
pub(crate) fn encoded(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in data.chunks(3) {
        let bits = chunk.iter().enumerate().fold(0_u32, |bits, (at, byte)| {
            bits | u32::from(*byte) << (16 - 8 * at)
        });
        for at in 0..4 {
            if at <= chunk.len() {
                text.push(char::from(ALPHABET[(bits >> (18 - 6 * at) & 63) as usize]));
            } else {
                text.push('=');
            }
        }
    }
    text
}

/// Every entry below `root` as the recorder listed it: its kind, a small
/// file's text or bytes and a larger one's size, and the mode of each of
/// `moded`.
pub(crate) fn listed(root: &Path, place: &Path, moded: &[&str]) -> Document {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("an entry").path();
            let relative = path
                .strip_prefix(root)
                .expect("below")
                .to_string_lossy()
                .into_owned();
            let metadata = path.symlink_metadata().expect("metadata");
            let mut fields: Vec<(String, Document)> = Vec::new();
            if metadata.file_type().is_symlink() {
                fields.push(("kind".to_owned(), "link".into()));
            } else if metadata.is_dir() {
                fields.push(("kind".to_owned(), "directory".into()));
                pending.push(path.clone());
            } else {
                let data = fs::read(&path).expect("a file");
                fields.push(("kind".to_owned(), "file".into()));
                if data.len() > 2048 {
                    fields.push(("size".to_owned(), data.len().into()));
                } else {
                    match String::from_utf8(data) {
                        Ok(text) => fields.push(("text".to_owned(), scrubbed(&text, place).into())),
                        Err(error) => {
                            let encoded = encoded(error.as_bytes());
                            fields.push(("base64".to_owned(), encoded.into()));
                        }
                    }
                }
            }
            if moded.contains(&relative.as_str()) {
                let mode = u64::from(metadata.permissions().mode() & 0o777);
                fields.push(("mode".to_owned(), mode.into()));
            }
            found.push((scrubbed(&relative, place), Document::Record(fields)));
        }
    }
    Document::Record(found).sorted()
}

/// The recording `bytes`.
pub(crate) fn recording(bytes: &[u8]) -> Document {
    Document::read(bytes).expect("a recording")
}

/// The text `name` of `value`.
pub(crate) fn text<'a>(value: &'a Document, name: &str) -> &'a str {
    let held = value.get(name).and_then(Document::text);
    held.unwrap_or_else(|| panic!("no text {name}"))
}

/// The items of the list `name` of `value`.
pub(crate) fn items<'a>(value: &'a Document, name: &str) -> &'a [Document] {
    let held = value.get(name).and_then(Document::items);
    held.unwrap_or_else(|| panic!("no list {name}"))
}

/// The whole number `name` of `value`.
pub(crate) fn whole(value: &Document, name: &str) -> i128 {
    let held = value.get(name).and_then(Document::whole);
    held.unwrap_or_else(|| panic!("no number {name}"))
}

/// How the recorded cases came out here: held alike, refused with the
/// reference's code or the vendor verifier's words, refused where the
/// reference's interpreter failed, and refused on purpose where the
/// reference held.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Tally {
    pub held: usize,
    pub refused: usize,
    pub raised: usize,
    pub stricter: usize,
}

impl Tally {
    /// Hold the case `case` called `name` to what came of it here: a held
    /// document, or the refusal said as text, the vendor verifier's words
    /// after `integrity: `. `raised` names the refusal
    /// for what the reference's interpreter raised, by its class and words,
    /// and `stricter` the cases the reference held that are refused here,
    /// each with its refusal.
    pub(crate) fn judge(
        &mut self,
        case: &Document,
        here: Result<Document, String>,
        raised: &dyn Fn(&str, &str) -> String,
        stricter: &[(&str, &str)],
    ) {
        let name = text(case, "name");
        let result = case.get("result").expect("a result");
        if let Some((_, refusal)) = stricter.iter().find(|(held, _)| *held == name) {
            assert!(result.get("held").is_some(), "{name} was held there");
            assert_eq!(here, Err((*refusal).to_owned()), "{name}");
            self.stricter += 1;
        } else if let Some(held) = result.get("held") {
            let came = here.unwrap_or_else(|refusal| panic!("{name}: refused {refusal}"));
            assert_eq!(came.sorted(), held.clone().sorted(), "{name}");
            self.held += 1;
        } else if let Some(code) = result.get("refused").and_then(Document::text) {
            assert_eq!(here, Err(code.to_owned()), "{name}");
            self.refused += 1;
        } else if let Some(words) = result.get("integrity").and_then(Document::text) {
            assert_eq!(here, Err(format!("integrity: {words}")), "{name}");
            self.refused += 1;
        } else {
            let (class, words) = (text(result, "raised"), text(result, "words"));
            assert_eq!(here, Err(raised(class, words)), "{name}: {class} {words}");
            self.raised += 1;
        }
    }
}

/// A `git show` of the recorded commit: an object of another revision, or
/// of a file it does not hold, ends with 1; one past the bound is refused as
/// the reference's bounded command refused it; any other ends with the
/// status the recording gives it, 0 where it gives none. A hook changes the
/// checkout once, when its object is first asked for.
pub(crate) struct Committed {
    root: PathBuf,
    revision: String,
    tracked: RefCell<BTreeMap<String, Vec<u8>>>,
    statuses: BTreeMap<String, i32>,
    hooks: RefCell<Vec<(Document, bool)>>,
    asked: RefCell<Vec<String>>,
}

impl Committed {
    /// The commit `case` records, of `revision`, for the checkout `root`
    /// of the case's place `place`.
    pub(crate) fn new(root: &Path, revision: &str, case: &Document, place: &Path) -> Self {
        let tracked = case
            .get("tracked")
            .and_then(Document::fields)
            .expect("a commit");
        let tracked = tracked
            .iter()
            .filter(|(_, spec)| *spec != Document::Null)
            .map(|(name, spec)| (name.clone(), bytes(spec, place)))
            .collect();
        let statuses = case
            .get("tracked")
            .and_then(Document::fields)
            .unwrap_or_default();
        let statuses = statuses
            .iter()
            .filter_map(|(name, spec)| {
                let status = spec.get("status").and_then(Document::whole)?;
                Some((name.clone(), i32::try_from(status).expect("a status")))
            })
            .collect();
        let hooks = case
            .get("hooks")
            .and_then(Document::items)
            .unwrap_or_default();
        Self {
            root: root.to_path_buf(),
            revision: revision.to_owned(),
            tracked: RefCell::new(tracked),
            statuses,
            hooks: RefCell::new(hooks.iter().map(|hook| (hook.clone(), false)).collect()),
            asked: RefCell::new(Vec::new()),
        }
    }

    /// What it was asked, as the recording lists it, since it was last
    /// asked this.
    pub(crate) fn taken(&self) -> Document {
        let asked = self.asked.take();
        Document::List(asked.into_iter().map(Document::from).collect())
    }
}

impl Shown for Committed {
    fn show(&self, root: &Path, object: &str, limit: u64) -> Sourced<(i32, Vec<u8>)> {
        assert_eq!(root, self.root, "git runs in the checkout");
        self.asked.borrow_mut().push(object.to_owned());
        for (hook, done) in self.hooks.borrow_mut().iter_mut() {
            if !*done && hook.get("when").and_then(Document::text) == Some(object) {
                *done = true;
                planted(root, hook.get("write").expect("what is written"));
                for (name, spec) in hook
                    .get("track")
                    .and_then(Document::fields)
                    .unwrap_or_default()
                {
                    let data = bytes(spec, root);
                    self.tracked.borrow_mut().insert(name.clone(), data);
                }
            }
        }
        let (revision, name) = object.split_once(':').expect("an object");
        let tracked = self.tracked.borrow();
        let Some(data) = tracked.get(name).filter(|_| revision == self.revision) else {
            return Ok((1, Vec::new()));
        };
        if data.len() as u64 > limit {
            return Err(Stop::Code("command_stdout_limit_exceeded"));
        }
        let status = self.statuses.get(name).copied().unwrap_or(0);
        Ok((status, data.clone()))
    }
}

#[test]
fn values_are_equal_as_the_references_interpreter_compares_them() {
    let read = |text: &str| Document::read(text.as_bytes()).expect("a document");
    for (left, right, equal) in [
        ("1", "1", true),
        ("1", "1.0", true),
        ("1.0", "1", true),
        ("true", "1", true),
        ("1", "true", true),
        ("false", "0", true),
        ("true", "true", true),
        ("true", "false", false),
        ("1", "2", false),
        ("1.5", "1.5", true),
        ("1.5", "2.5", false),
        ("2", "1.5", false),
        ("\"1\"", "1", false),
        ("null", "null", true),
        ("null", "0", false),
        ("0", "null", false),
        ("\"a\"", "\"a\"", true),
        ("\"a\"", "\"b\"", false),
        ("[1, \"a\"]", "[1.0, \"a\"]", true),
        ("[1]", "[1, 1]", false),
        ("[1, 2]", "[2, 1]", false),
        ("[]", "{}", false),
        ("{\"a\": 1, \"b\": 2}", "{\"b\": 2, \"a\": 1}", true),
        ("{\"a\": 1}", "{\"a\": 1, \"b\": 2}", false),
        ("{\"a\": 1, \"b\": 2}", "{\"a\": 1}", false),
        ("{\"a\": 1}", "{\"b\": 1}", false),
        ("{\"a\": 1}", "{\"a\": 2}", false),
        ("{\"a\": {\"b\": true}}", "{\"a\": {\"b\": 1}}", true),
        ("\"x\"", "[\"x\"]", false),
    ] {
        assert_eq!(alike(&read(left), &read(right)), equal, "{left} {right}");
    }
}

#[test]
fn text_is_split_as_the_references_interpreter_splits_it() {
    // What the reference's `splitlines` and `split` answered for each.
    for (text, expected) in [
        ("", vec![]),
        ("a", vec!["a"]),
        ("a\n", vec!["a"]),
        ("a\n\n", vec!["a", ""]),
        ("\n", vec![""]),
        ("a\r\nb", vec!["a", "b"]),
        ("a\rb\n", vec!["a", "b"]),
        ("a\n\rb", vec!["a", "", "b"]),
        ("a\u{b}b\u{c}c", vec!["a", "b", "c"]),
        (
            "a\u{1c}b\u{1d}c\u{1e}d\u{1f}e",
            vec!["a", "b", "c", "d\u{1f}e"],
        ),
        ("a\u{85}b\u{2028}c\u{2029}d", vec!["a", "b", "c", "d"]),
        ("a\u{a0}b", vec!["a\u{a0}b"]),
        ("a\r", vec!["a"]),
        ("é\né", vec!["é", "é"]),
    ] {
        assert_eq!(lines(text), expected, "{text:?}");
    }
    for (text, expected) in [
        ("", vec![]),
        ("  ", vec![]),
        ("a b", vec!["a", "b"]),
        (
            " a\t\nb\u{1c}c\u{a0}d\u{3000}e ",
            vec!["a", "b", "c", "d", "e"],
        ),
        ("a\u{200b}b", vec!["a\u{200b}b"]),
    ] {
        assert_eq!(words(text), expected, "{text:?}");
    }
}
