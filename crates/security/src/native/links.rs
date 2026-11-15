//! The static libraries that the build scripts said they link: exactly
//! those of the reviewed record, each found once in the search paths that
//! the script gave and where the record expects its kind.

use super::{
    Build,
    artifact::record_hash,
    build::{CargoBuild, beneath, package_name, resolved},
    field, object, problem, strings,
};
use crate::{
    document::Document,
    vendor::{Verdict, manifest},
};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

fn text<'a>(record: &'a Document, name: &str) -> Verdict<&'a str> {
    manifest::string(field(record, name)?)
}

/// The one static archive of `library` in the search `paths`, which must
/// lie where its kind belongs: the external library in its installation,
/// OpenSSL in its own, the C++ runtime below `system`, and anything else
/// in the directory of the build script.
fn resolve_archive(
    build: &Build<'_>,
    paths: &[PathBuf],
    library: &str,
    place: (&Path, &Path),
) -> Verdict<PathBuf> {
    let name = format!("lib{library}.a");
    let found = paths.iter().map(|path| path.join(&name));
    let found: BTreeSet<PathBuf> = found
        .filter(|path| path.is_file())
        .map(|path| resolved(&path))
        .collect();
    let mut found = found.into_iter();
    let (Some(selected), None) = (found.next(), found.next()) else {
        return Err(problem("Missing or ambiguous native static archive"));
    };
    let (out_dir, system) = place;
    match library {
        "crypto-awslc" => beneath(&selected, &build.aws_lc_prefix.join("lib")),
        "ssl" | "crypto" => beneath(&selected, &build.openssl_prefix.join("lib")),
        "stdc++" => beneath(&selected, system),
        _ => beneath(&selected, out_dir),
    }
}

/// The record of every static library that the build linked, with the
/// provider that linked it and the directory of its build script, and the
/// C++ runtime among them. The `system` root is where that runtime
/// belongs.
pub fn link_archives(
    build: &Build<'_>,
    graph: &CargoBuild,
    manifest: &Document,
    system: &Path,
) -> Verdict<(Vec<Document>, PathBuf)> {
    let providers = object(field(manifest, "native_links")?)?;
    let mut seen = BTreeSet::new();
    let (mut records, mut runtime) = (Vec::new(), None);
    for ((identifier, _), event) in &graph.scripts {
        let libraries = strings(field(event, "linked_libs")?)?;
        if libraries.is_empty() {
            continue;
        }
        let name = package_name(identifier)?;
        let provider = providers.get(name);
        let provider = provider.filter(|_| graph.artifacts.contains_key(identifier));
        let provider = provider
            .ok_or_else(|| problem("Unlisted native link provider or missing compiler artifact"))?;
        seen.insert(name);
        let provider = object(provider)?;
        let expected = strings(field(provider, "static")?)?;
        let system_libraries = strings(field(provider, "system")?)?;
        let statics: Vec<String> = expected
            .iter()
            .map(|library| format!("static={library}"))
            .collect();
        let allowed = |library: &&str| {
            statics.iter().any(|expected| expected == library) || system_libraries.contains(library)
        };
        let declared = |expected: &String| libraries.contains(&expected.as_str());
        if !libraries.iter().all(allowed) || !statics.iter().all(declared) {
            return Err(problem(
                "Native link declarations differ from reviewed policy",
            ));
        }
        let directory = build.root.join("target/release/build");
        let out_dir = beneath(Path::new(text(event, "out_dir")?), &directory)?;
        let mut paths = Vec::new();
        for path in strings(field(event, "linked_paths")?)? {
            paths.push(PathBuf::from(path.strip_prefix("native=").unwrap_or(path)));
        }
        if !paths.iter().all(|path| path.is_absolute()) {
            return Err(problem("Native linker search path must be absolute"));
        }
        for library in expected {
            let selected = resolve_archive(build, &paths, library, (&out_dir, system))?;
            let record = record_hash(&selected, true)?;
            let mut record = record.fields().unwrap_or_default().to_vec();
            record.push(("provider".to_owned(), name.into()));
            record.push(("library".to_owned(), library.into()));
            record.push(("out_dir".to_owned(), out_dir.display().to_string().into()));
            records.push(Document::Record(record));
            if library == "stdc++" {
                runtime = Some(selected);
            }
        }
    }
    let named: BTreeSet<&str> = providers
        .fields()
        .unwrap_or_default()
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    match runtime.filter(|_| seen == named) {
        Some(runtime) => Ok((records, runtime)),
        None => Err(problem(
            "Missing native link provider or static C++ runtime",
        )),
    }
}

#[cfg(test)]
#[path = "links_tests.rs"]
mod tests;
