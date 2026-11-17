//! Paths as the reference's interpreter spelled and resolved them: a path
//! that names a place is held to what its resolution makes of it, so that
//! no link and no parent step on the way can stand in for another place.

use std::{
    collections::HashMap,
    ffi::OsStr,
    os::unix::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
};

/// `path` as the reference's paths spell one: without repeated separators,
/// current-directory steps or a final separator, with exactly two leading
/// separators kept, and `.` for nothing.
pub(crate) fn spelled(path: &[u8]) -> Vec<u8> {
    let root: &[u8] = if !path.starts_with(b"/") {
        b""
    } else if path.starts_with(b"//") && !path.starts_with(b"///") {
        b"//"
    } else {
        b"/"
    };
    let steps = path.split(|byte| *byte == b'/');
    let steps: Vec<&[u8]> = steps
        .filter(|step| !step.is_empty() && *step != b".")
        .collect();
    let mut text = root.to_vec();
    text.extend(steps.join(&b'/'));
    if text.is_empty() {
        text.push(b'.');
    }
    text
}

/// The path `given` as the reference's paths spell one.
pub(crate) fn path(given: &OsStr) -> PathBuf {
    PathBuf::from(OsStr::from_bytes(&spelled(given.as_bytes())))
}

/// What the reference's resolution makes of the absolute path `spelled`,
/// step by step as its interpreter takes one: a parent step goes back, a
/// link is replaced by its target, a link met again while it is resolved is
/// kept as it is, and a step that cannot be looked at is kept as it is
/// spelled.
pub(crate) fn resolved(spelled: &[u8]) -> Vec<u8> {
    // A step of nothing marks a resolved link, which is the step under it.
    let mut rest: Vec<Option<Vec<u8>>> = spelled
        .split(|byte| *byte == b'/')
        .rev()
        .map(|step| Some(step.to_vec()))
        .collect();
    let mut steps = rest.len();
    let mut path = b"/".to_vec();
    let mut seen: HashMap<Vec<u8>, Option<Vec<u8>>> = HashMap::new();
    while steps > 0 {
        let Some(step) = rest.pop() else { break };
        let Some(name) = step else {
            if let Some(Some(link)) = rest.pop() {
                seen.insert(link, Some(path.clone()));
            }
            continue;
        };
        steps -= 1;
        if name.is_empty() || name == b"." {
            continue;
        }
        if name == b".." {
            let cut = path.iter().rposition(|byte| *byte == b'/').unwrap_or(0);
            path.truncate(cut);
            if path.is_empty() {
                path.push(b'/');
            }
            continue;
        }
        let mut next = path.clone();
        if next != b"/" {
            next.push(b'/');
        }
        next.extend_from_slice(&name);
        let looked = std::fs::symlink_metadata(OsStr::from_bytes(&next));
        if !looked.is_ok_and(|metadata| metadata.file_type().is_symlink()) {
            path = next;
            continue;
        }
        match seen.get(&next) {
            Some(Some(target)) => {
                path.clone_from(target);
                continue;
            }
            Some(None) => {
                path = next;
                continue;
            }
            None => {}
        }
        let Ok(target) = std::fs::read_link(OsStr::from_bytes(&next)) else {
            path = next;
            continue;
        };
        let target = target.into_os_string().into_vec();
        if target.starts_with(b"/") {
            path = b"/".to_vec();
        }
        seen.insert(next.clone(), None);
        rest.push(Some(next));
        rest.push(None);
        let parts = target.split(|byte| *byte == b'/');
        let parts: Vec<Option<Vec<u8>>> = parts.map(|part| Some(part.to_vec())).collect();
        steps += parts.len();
        rest.extend(parts.into_iter().rev());
    }
    path
}

/// Whether `directory` is absolute and already what the reference's
/// resolution makes of it, as the directory of a kept build must be: no
/// parent step and no link on the way to it, however much of it is there.
pub fn canonical(directory: &Path) -> bool {
    let spelled = spelled(directory.as_os_str().as_bytes());
    directory.is_absolute() && resolved(&spelled) == spelled
}

/// Whether `path` resolves to itself made absolute, as the reference held
/// a source's path: a relative path is taken from the working directory,
/// and one that is neither is refused.
pub(crate) fn unaliased(path: &Path) -> bool {
    let mut absolute = spelled(path.as_os_str().as_bytes());
    if !absolute.starts_with(b"/") {
        let Ok(directory) = std::env::current_dir() else {
            return false;
        };
        let mut whole = directory.into_os_string().into_vec();
        whole.push(b'/');
        whole.extend_from_slice(&absolute);
        absolute = spelled(&whole);
    }
    resolved(&absolute) == absolute
}

#[cfg(test)]
mod tests {
    use super::{canonical, path, resolved, spelled, unaliased};
    use crate::{document::Document, scratch::Scratch};
    use std::{
        ffi::OsStr,
        os::unix::{ffi::OsStrExt, fs::symlink},
        path::Path,
    };

    const RECORDED: &[u8] = include_bytes!("recorded_paths.json");

    #[test]
    fn every_recorded_path_is_resolved_as_the_reference_resolved_it() {
        // What the reference's `Path.resolve()` made of each, on the
        // recorded tree of links.
        let recorded = Document::read(RECORDED).expect("a recording");
        let scratch = Scratch::new("resolved-paths");
        let place = scratch.0.to_string_lossy().into_owned();
        crate::codeql::tests::planted(&scratch.0, recorded.get("tree").expect("a tree"));
        let cases = recorded
            .get("cases")
            .and_then(Document::items)
            .expect("cases");
        let mut held = 0;
        for case in cases {
            let text = |name: &str| case.get(name).and_then(Document::text).expect("a text");
            let given = text("path").replace("<place>", &place);
            let made = resolved(&spelled(given.as_bytes()));
            let made = String::from_utf8(made)
                .expect("text")
                .replace(&place, "<place>");
            assert_eq!(made, text("resolved"), "{given}");
            let kept = case.get("canonical") == Some(&Document::Bool(true));
            assert_eq!(
                canonical(Path::new(OsStr::from_bytes(given.as_bytes()))),
                kept,
                "{given}"
            );
            held += usize::from(kept);
        }
        assert_eq!((cases.len(), held), (30, 2));
    }

    #[test]
    fn a_path_is_spelled_and_held_to_its_resolution_as_the_reference_held_it() {
        // What the reference's paths made of each.
        for (given, spelled) in [
            ("", "."),
            ("./", "."),
            ("a//b/", "a/b"),
            ("/a/./b", "/a/b"),
            ("//a", "//a"),
            ("///a", "/a"),
            ("/", "/"),
        ] {
            assert_eq!(path(OsStr::new(given)), Path::new(spelled), "{given}");
        }
        let scratch = Scratch::new("unaliased");
        let root = &scratch.0;
        std::fs::create_dir(root.join("real")).expect("a directory");
        symlink(root.join("real"), root.join("link")).expect("a link");
        assert!(unaliased(&root.join("real/x")));
        assert!(!unaliased(&root.join("link/x")));
        assert!(!unaliased(&root.join("real/../real/x")));
        // A relative path is taken from the working directory, which is the
        // crate's while its tests run.
        assert!(unaliased(Path::new("src/paths.rs")));
        assert!(unaliased(Path::new("./src//paths.rs")));
        assert!(!unaliased(Path::new("src/../src/paths.rs")));
    }
}
