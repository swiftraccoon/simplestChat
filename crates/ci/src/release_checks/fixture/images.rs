//! The three fixture releases derived from the one checked production image.
//! Each differs only in synthetic public browser files, so two editions can
//! be told apart without rebuilding application code; the baseline lacks the
//! version endpoint, and the failed edition replaces the server with a fixed
//! immediate exit.

use super::{files::sha256, plan::FAILURE_MARKER};
use std::{collections::BTreeMap, path::Path};

/// The three derived releases, in the order the fixture builds them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum Kind {
    Baseline,
    Candidate,
    Failed,
}
impl Kind {
    pub(super) const ALL: [Self; 3] = [Self::Baseline, Self::Candidate, Self::Failed];
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Candidate => "candidate",
            Self::Failed => "failed",
        }
    }
}

/// One derived release: its synthetic revision, local tag and, once built,
/// its immutable image identity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Derived {
    pub revision: String,
    pub tag: String,
    pub image: String,
}

/// The synthetic revision of `kind` for the run `token`.
pub(super) fn revision(token: &str, kind: Kind) -> String {
    sha256(format!("disposable-release:{token}:{}", kind.name()).as_bytes())[..40].to_owned()
}

pub(super) fn tag(revision: &str) -> String {
    format!("simplestchat-release/production:{revision}")
}

/// The build context of `kind` by file name, the `Dockerfile` included.
pub(super) fn context(base_tag: &str, token: &str, kind: Kind) -> BTreeMap<String, String> {
    let revision = revision(token, kind);
    let mut files = BTreeMap::new();
    let mut dockerfile = format!(
        "FROM {base_tag}\nLABEL org.opencontainers.image.revision={revision}\n\
         LABEL {}={token}\n",
        super::plan::LABEL
    );
    let chunk = format!("export const revision = '{revision}';\n");
    let chunk_name = format!("rollover-{}.js", &sha256(chunk.as_bytes())[..16]);
    let entry = format!(
        "globalThis.releaseRollover = {{revision:'{revision}',identity:crypto.randomUUID(),\
         chunk:'/assets/{chunk_name}',load:async()=> (await import('/assets/{chunk_name}')).\
         revision}};\n"
    );
    let entry_name = format!("rollover-entry-{}.js", &sha256(entry.as_bytes())[..16]);
    let html = format!(
        "<!doctype html><title>Release asset fixture</title>\
         <script src=\"/assets/{entry_name}\"></script>"
    );
    dockerfile.push_str(&format!(
        "COPY --chown=10001:10001 {chunk_name} /app/web/dist/assets/{chunk_name}\n\
         COPY --chown=10001:10001 {entry_name} /app/web/dist/assets/{entry_name}\n\
         COPY --chown=10001:10001 asset-rollover.html /app/web/dist/asset-rollover.html\n"
    ));
    files.insert(chunk_name, chunk);
    files.insert(entry_name, entry);
    files.insert("asset-rollover.html".into(), html);
    if kind == Kind::Baseline {
        dockerfile.push_str("USER 0\nRUN rm -f /app/web/dist/app-version.json\nUSER 10001:10001\n");
    } else {
        files.insert(
            "app-version.json".into(),
            format!("{{\n  \"revision\": \"{revision}\"\n}}\n"),
        );
        dockerfile
            .push_str("COPY --chown=10001:10001 app-version.json /app/web/dist/app-version.json\n");
    }
    if kind == Kind::Failed {
        files.insert(
            "failed-server".into(),
            format!("#!/bin/sh\necho '{FAILURE_MARKER}' >&2\nexit 42\n"),
        );
        dockerfile
            .push_str("COPY --chown=10001:10001 --chmod=0755 failed-server /app/simplestChat\n");
    }
    files.insert("Dockerfile".into(), dockerfile);
    files
}

/// The SHA-384 of every migration under `directory`, by its numeric version.
pub(super) fn migrations(directory: &Path) -> std::io::Result<BTreeMap<String, String>> {
    use sha2::{Digest, Sha384};
    let mut found = BTreeMap::new();
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.ends_with(".sql") {
            continue;
        }
        let version = name
            .split('_')
            .next()
            .and_then(|prefix| prefix.parse::<u64>().ok())
            .ok_or_else(|| std::io::Error::other("migration without a version"))?;
        let bytes = std::fs::read(&path)?;
        let digest = super::files::hex(&Sha384::digest(&bytes));
        if found.insert(version.to_string(), digest).is_some() {
            return Err(std::io::Error::other("repeated migration version"));
        }
    }
    Ok(found)
}
