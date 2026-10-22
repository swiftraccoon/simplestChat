//! The reference helper's tagged C++ selection, applied only to its fresh copy.

use super::plan::failure;
use crate::tooling::Result;
use std::collections::{BTreeMap, BTreeSet};

const TAGS: [&str; 3] = ["[dtls]", "[media-diagnostics]", "[bandwidth-constraints]"];
const ANCHOR: &str = "mediasoup_worker_test = executable(";

fn text(bytes: &[u8]) -> Result<String> {
    // Python's read_text uses universal newlines, including an isolated CR.
    Ok(std::str::from_utf8(bytes)
        .map_err(|_| failure(1))?
        .replace("\r\n", "\n")
        .replace('\r', "\n"))
}
pub(super) fn meson(files: &BTreeMap<String, Vec<u8>>) -> Result<Vec<u8>> {
    let mut selected = BTreeSet::from([
        "test/src/tests.cpp".to_owned(),
        "test/src/testHelpers.cpp".to_owned(),
    ]);
    let mut found = BTreeSet::new();
    for (name, bytes) in files {
        if name.starts_with("test/src/") && name.ends_with(".cpp") {
            let source = text(bytes)?;
            let matching: Vec<_> = TAGS.iter().filter(|tag| source.contains(**tag)).collect();
            if !matching.is_empty() {
                selected.insert(name.clone());
                found.extend(matching.into_iter().copied());
            }
        }
    }
    if found != BTreeSet::from(TAGS)
        || selected.iter().any(|name| {
            !name.starts_with("test/src/")
                || !name.ends_with(".cpp")
                || name.len() <= "test/src/.cpp".len()
                || !name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_/.-".contains(&byte))
        })
    {
        return Err(failure(1));
    }
    let definition = text(files.get("meson.build").ok_or_else(|| failure(1))?)?;
    if definition.matches(ANCHOR).count() != 1 {
        return Err(failure(1));
    }
    let mut selection = String::from("test_sources = [\n");
    for name in selected {
        selection.push_str(&format!("  '{name}',\n"));
    }
    selection.push_str("]\n\n");
    Ok(definition
        .replace(ANCHOR, &(selection + ANCHOR))
        .into_bytes())
}

pub(super) fn assertions(dtls: &[u8], orderly: &[u8]) -> Result<()> {
    if text(orderly)?.contains("RTC::DtlsTransport::")
        || !text(dtls)?.contains(concat!(
            "producer:11112233-4455-6677-8899-aabbccddeeff, ",
            "transport:00112233-4455-6677-8899-aabbccddeeff, kind:audio, encodingIdx:2, ",
            "workerMs:2500, timeoutMs:1500, mediaObserved:1, lastMediaMs:1000, paused:0"
        ))
    {
        return Err(failure(1));
    }
    Ok(())
}

#[cfg(test)]
#[path = "source_tests.rs"]
mod tests;
