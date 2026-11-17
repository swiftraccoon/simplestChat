//! The wrap files of the native build against the record: each pins one
//! archive by its address and digest, with a pinned remote patch or a
//! local overlay, and nothing fetches by another way.
//!
//! A wrap file is read as the reference's library read it: sections in
//! brackets, a name and its value around the first `=` or `:`, names in
//! lowercase, lines that begin with `#` or `;` as comments, and a line
//! indented deeper than its name as more of the value.

use super::{
    Verdict,
    manifest::{Manifest, Wrap, relative_path},
    problem,
};
use crate::policy::space;
use std::collections::BTreeMap;

/// The names a wrap may give a value to.
const ALLOWED: [&str; 10] = [
    "directory",
    "source_url",
    "source_filename",
    "source_hash",
    "patch_url",
    "patch_filename",
    "patch_hash",
    "patch_directory",
    "source_fallback_url",
    "wrapdb_version",
];

/// The values of one section, by their names.
type Section = Vec<(String, Vec<String>)>;

/// The sections of a wrap file with their values, and the section of
/// defaults. Nothing for a file that the reference's library refuses: a
/// value before any section, a line that is neither, and a section or a
/// name that is given twice.
fn sections(text: &str) -> Option<(Vec<(String, Section)>, Section)> {
    let mut sections: Vec<(String, Section)> = Vec::new();
    let mut defaults: Section = Vec::new();
    // Where the values go: nowhere yet, the defaults, or a section.
    let mut current: Option<Option<usize>> = None;
    let mut named = false;
    let mut indent = 0;
    let mut sound = true;
    for line in text.split_inclusive('\n') {
        let value = line.trim_matches(space);
        let last = |sections: &mut Vec<(String, Section)>, defaults: &mut Section, more: &str| {
            let section = match current {
                Some(Some(at)) => sections.get_mut(at).map(|(_, section)| section),
                Some(None) => Some(defaults),
                None => None,
            };
            if let Some((_, value)) = section.and_then(|section| section.last_mut()) {
                value.push(more.to_owned());
            }
        };
        if value.starts_with(['#', ';']) {
            continue;
        }
        if value.is_empty() {
            if named {
                last(&mut sections, &mut defaults, "");
            }
            continue;
        }
        let depth = line.find(|character| !space(character)).unwrap_or(0);
        if current.is_some() && named && depth > indent {
            last(&mut sections, &mut defaults, value);
            continue;
        }
        indent = depth;
        let header = value
            .strip_prefix('[')
            .and_then(|rest| rest.rfind(']').map(|end| &rest[..end]));
        if let Some(header) = header.filter(|header| !header.is_empty()) {
            if sections.iter().any(|(name, _)| name == header) {
                return None;
            }
            if header == "DEFAULT" {
                current = Some(None);
            } else {
                sections.push((header.to_owned(), Vec::new()));
                current = Some(Some(sections.len() - 1));
            }
            named = false;
            continue;
        }
        let place = current?;
        let Some(at) = value.find(['=', ':']) else {
            sound = false;
            continue;
        };
        let name = value[..at].trim_end_matches(space).to_lowercase();
        sound &= !name.is_empty();
        let section = match place {
            Some(at) => &mut sections.get_mut(at)?.1,
            None => &mut defaults,
        };
        if section.iter().any(|(held, _)| *held == name) {
            return None;
        }
        named = !name.is_empty();
        section.push((name, vec![value[at + 1..].trim_matches(space).to_owned()]));
    }
    sound.then_some((sections, defaults))
}

/// The value of `name` in the section `section` of the wrap file `text`,
/// or of the defaults where the section has none, as the reference's
/// library answers it: its lines joined, with no white space at its end.
/// The outer nothing is a file that library refuses, the inner one a
/// section or a value that is not there.
pub(crate) fn value(text: &str, section: &str, name: &str) -> Option<Option<String>> {
    let (sections, defaults) = sections(text)?;
    let Some((_, values)) = sections.iter().find(|(held, _)| held == section) else {
        return Some(None);
    };
    let found = values
        .iter()
        .chain(&defaults)
        .find(|(held, _)| held == name);
    Some(found.map(|(_, lines)| lines.join("\n").trim_end_matches(space).to_owned()))
}

/// Hold the wrap file `text` of `wrap` to the record: one pinned file
/// source, with what the record says of its fallback, its remote patch and
/// its local overlay, whose files must be among `local`.
pub(crate) fn validate<'a>(
    wrap: &Wrap,
    manifest: &Manifest,
    text: &[u8],
    mut local: impl Iterator<Item = &'a str>,
) -> Verdict<()> {
    let path = &wrap.path;
    let refused = |words: &str| problem(format!("{words}: {path}"));
    let text = std::str::from_utf8(text).map_err(|_| refused("Native wrap is no UTF-8 text"))?;
    let (sections, defaults) =
        sections(text).ok_or_else(|| refused("Native wrap is no configuration"))?;
    let defaults = !defaults.is_empty();
    let pinned = sections.iter().find(|(name, _)| name == "wrap-file");
    let others = sections
        .iter()
        .any(|(name, _)| name != "wrap-file" && name != "provide");
    let Some((_, section)) = pinned.filter(|_| !defaults && !others) else {
        return Err(refused("Only pinned wrap-file dependencies are allowed"));
    };
    let values: BTreeMap<&str, String> = section
        .iter()
        .map(|(name, lines)| {
            (
                name.as_str(),
                lines.join("\n").trim_end_matches(space).to_owned(),
            )
        })
        .collect();
    if values.keys().any(|name| !ALLOWED.contains(name)) {
        return Err(refused("Unsupported native wrap fields"));
    }
    let value = |name: &str| values.get(name).map(String::as_str);
    let source = manifest.source(&wrap.source);
    let source = source.ok_or_else(|| refused("Native source pin differs"))?;
    if value("source_url") != Some(&source.url) || value("source_hash") != Some(&source.sha256) {
        return Err(refused("Native source pin differs"));
    }
    relative_path(value("directory").unwrap_or_default())?;
    relative_path(value("source_filename").unwrap_or_default())?;
    let fallbacks: Vec<&str> = value("source_fallback_url").into_iter().collect();
    if fallbacks != wrap.fallback_urls {
        return Err(refused("Native fallback pin differs"));
    }
    if let Some(patch) = &wrap.patch_source {
        let patch = manifest.source(patch);
        let pinned = patch.is_some_and(|patch| {
            value("patch_url") == Some(&patch.url) && value("patch_hash") == Some(&patch.sha256)
        });
        if wrap.patch_directory.is_some() || !pinned {
            return Err(refused("Native patch pin differs"));
        }
        relative_path(value("patch_filename").unwrap_or_default())?;
    } else if ["patch_url", "patch_hash", "patch_filename"]
        .iter()
        .any(|name| value(name).is_some())
    {
        return Err(refused("Unlisted remote native patch"));
    }
    if value("patch_directory") != wrap.patch_directory.as_deref() {
        return Err(refused("Native patch directory differs"));
    }
    if let Some(directory) = &wrap.patch_directory {
        let parent = path.rsplit_once('/').map_or("", |(parent, _)| parent);
        let overlay = if parent.is_empty() {
            format!("packagefiles/{directory}/")
        } else {
            format!("{parent}/packagefiles/{directory}/")
        };
        if !local.any(|path| path.starts_with(&overlay)) {
            return Err(refused("Missing local native overlay"));
        }
    }
    Ok(())
}
