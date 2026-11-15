//! The OpenSSL that was built: its headers say the reviewed version and
//! that it can load neither modules nor engines, and both providers linked
//! the same static libraries of that one installation.

use super::{artifact::record_hash, field, licenses::space, object, problem, read, text};
use crate::{
    digest,
    document::Document,
    vendor::{Problem, Verdict, manifest},
};
use std::{collections::BTreeSet, path::Path};

/// What the installation must record as disabled, line by line.
const DISABLED: [&str; 4] = ["shared", "dso", "module", "engine"];
/// The crates that link OpenSSL.
const PROVIDERS: [&str; 2] = ["mediasoup-sys", "openssl-sys"];
/// The most archives one library may be linked from.
const MAX_RECORDS: usize = 20000;

/// Where the next thing begins after any spaces at `at`.
fn spaces(text: &str, at: usize) -> usize {
    let rest = &text[at..];
    at + rest.len() - rest.trim_start_matches(space).len()
}

/// Where `word` ends when it stands at `at`.
fn word(text: &str, at: usize, word: &str) -> Option<usize> {
    text[at..].starts_with(word).then_some(at + word.len())
}

/// Where a definition of `macro_name` without a value ends when one
/// begins at the line start `at`: spaces, lines among them, may stand
/// before the hash, after it and before the name, and nothing but spaces
/// may follow the name on its line.
fn definition(text: &str, at: usize, macro_name: &str) -> Option<usize> {
    let hash = word(text, spaces(text, at), "#")?;
    let define = word(text, spaces(text, hash), "define")?;
    let name = spaces(text, define);
    let named = word(text, name, macro_name).filter(|_| name > define)?;
    let line = text[named..]
        .find('\n')
        .map_or(text.len(), |end| named + end);
    text[named..line].chars().all(space).then(|| {
        // What is matched reaches the last line end among the spaces that follow.
        let after = spaces(text, named);
        text[named..after]
            .rfind('\n')
            .map_or(after.min(line), |end| named + end)
    })
}

/// How many definitions of `macro_name` without a value the text has.
fn definitions(text: &str, macro_name: &str) -> usize {
    let (mut count, mut from) = (0, 0);
    let starts = std::iter::once(0).chain(text.match_indices('\n').map(|(at, _)| at + 1));
    for start in starts {
        if start < from {
            continue;
        }
        if let Some(end) = definition(text, start, macro_name) {
            count += 1;
            from = end;
        }
    }
    count
}

/// Whether the text takes `macro_name` back anywhere.
fn undefined(text: &str, macro_name: &str) -> bool {
    let taken = |(at, _): (usize, &str)| {
        let undef = word(text, spaces(text, at + 1), "undef")?;
        let name = spaces(text, undef);
        let named = word(text, name, macro_name).filter(|_| name > undef)?;
        let next = text[named..].chars().next();
        let ends = !next.is_some_and(|next| next.is_alphanumeric() || next == '_');
        ends.then_some(())
    };
    text.match_indices('#').any(|found| taken(found).is_some())
}

/// A small file of the installation, with its record.
fn installed(path: &Path) -> Verdict<(Vec<u8>, Document)> {
    let body = read(path)?;
    let record = Document::record([
        ("path", path.display().to_string().into()),
        ("sha256", digest(&body).into()),
        ("size", body.len().into()),
    ]);
    Ok((body, record))
}

fn readable<'a>(body: &'a [u8], path: &Path) -> Verdict<&'a str> {
    let text = std::str::from_utf8(body);
    text.map_err(|_| Problem(format!("No text: {}", path.display())))
}

/// The evidence of the OpenSSL installed at `prefix` for the reviewed
/// record `component`, against the `archives` that the build linked.
pub fn openssl_build_evidence(
    prefix: &Path,
    component: &Document,
    archives: &[Document],
) -> Verdict<Document> {
    let version_path = prefix.join("include/openssl/opensslv.h");
    let configuration_path = prefix.join("include/openssl/configuration.h");
    let disabled_path = prefix.join("share/simplestchat/openssl-disabled.txt");
    let (header, version_record) = installed(&version_path)?;
    let versioned = format!(
        "# define OPENSSL_VERSION_STR \"{}\"",
        text(component, "version")?
    );
    if !readable(&header, &version_path)?.contains(&versioned) {
        return Err(problem(
            "Built OpenSSL headers differ from the declared source version",
        ));
    }
    let (configuration, configuration_record) = installed(&configuration_path)?;
    let configured = readable(&configuration, &configuration_path)?;
    for macro_name in ["OPENSSL_NO_DSO", "OPENSSL_NO_ENGINE"] {
        if definitions(configured, macro_name) != 1 || undefined(configured, macro_name) {
            return Err(problem("Built OpenSSL permits dynamic loading or engines"));
        }
    }
    let (disabled, disabled_record) = installed(&disabled_path)?;
    if disabled != format!("{}\n", DISABLED.join("\n")).as_bytes() {
        return Err(problem(
            "Built OpenSSL configuration does not disable modules",
        ));
    }
    let mut libraries = Vec::new();
    let mut previous: Option<BTreeSet<(&str, &str)>> = None;
    for library in ["ssl", "crypto"] {
        let actual = record_hash(&prefix.join("lib").join(format!("lib{library}.a")), true)?;
        let mut linked = Vec::new();
        for archive in archives {
            if field(object(archive)?, "library")?.text() == Some(library) {
                linked.push(archive);
            }
        }
        let mut instances = BTreeSet::new();
        for archive in &linked {
            let part = |name: &str| manifest::string(field(archive, name)?);
            instances.insert((part("provider")?, part("out_dir")?));
        }
        let providers: BTreeSet<&str> = instances.iter().map(|(provider, _)| *provider).collect();
        let mut same = true;
        for archive in &linked {
            for (name, value) in actual.fields().unwrap_or_default() {
                same &= field(archive, name)? == value;
            }
        }
        let agreed = linked.len() <= MAX_RECORDS
            && instances.len() == linked.len()
            && providers == PROVIDERS.into_iter().collect()
            && previous
                .as_ref()
                .is_none_or(|previous| *previous == instances)
            && same;
        if !agreed {
            return Err(problem(
                "OpenSSL configuration and static link archives differ",
            ));
        }
        previous = Some(instances);
        let mut record = vec![("library".to_owned(), library.into())];
        record.extend(actual.fields().unwrap_or_default().iter().cloned());
        libraries.push(Document::Record(record));
    }
    let disabled_options = DISABLED.into_iter().map(Document::from).collect();
    let built = Document::record([
        ("version_header", version_record),
        ("configuration_header", configuration_record),
        ("disabled_options_record", disabled_record),
        ("disabled_options", Document::List(disabled_options)),
        ("static_libraries", Document::List(libraries)),
    ]);
    let mut evidence = object(component)?.fields().unwrap_or_default().to_vec();
    evidence.retain(|(name, _)| name != "build");
    evidence.push(("build".to_owned(), built));
    Ok(Document::Record(evidence))
}

#[cfg(test)]
#[path = "openssl_tests.rs"]
mod tests;
