//! Fixed CI inputs; ordinary local invocations never select an ambient engine.

use crate::tooling::{Environment, Failure, Result};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(super) struct Context {
    pub root: PathBuf,
    pub environment: Environment,
    pub temporary: PathBuf,
    pub revision: String,
    pub image: String,
    pub local_arm: bool,
    pub cache_hit: bool,
}

fn text<'a>(environment: &'a Environment, key: &str) -> &'a str {
    environment
        .get(OsStr::new(key))
        .and_then(|value| value.to_str())
        .unwrap_or("")
}

pub(super) fn digest(value: &str) -> bool {
    value
        .strip_prefix("sha256:")
        .is_some_and(|hash| hash.len() == 64 && lower_hex(hash))
}

fn lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

impl Context {
    pub(super) fn new(
        root: &Path,
        environment: Environment,
        system: &str,
        architecture: &str,
        verify: bool,
    ) -> Result<Self> {
        let local = text(&environment, "ACT") == "true";
        let authorized = if local {
            text(&environment, "LOCAL_CI_DISPOSABLE") == "1"
        } else {
            text(&environment, "GITHUB_ACTIONS") == "true"
                && text(&environment, "RUNNER_ENVIRONMENT") == "github-hosted"
        };
        if system != "Linux" || !authorized {
            return Err(Failure::new(
                2,
                "Production images require hosted GitHub or disposable local CI.",
            ));
        }
        let revision = text(&environment, "GITHUB_SHA").to_owned();
        if revision.len() != 40 || !lower_hex(&revision) {
            return Err(Failure::new(
                2,
                "Production image revision must be a canonical Git SHA.",
            ));
        }
        let temporary_text = text(&environment, "RUNNER_TEMP");
        if temporary_text.bytes().any(|byte| b",\r\n".contains(&byte)) {
            return Err(Failure::new(
                2,
                "RUNNER_TEMP cannot contain cache-option delimiters.",
            ));
        }
        let temporary = PathBuf::from(temporary_text);
        if !temporary.is_absolute() || temporary.is_symlink() || !temporary.is_dir() {
            return Err(Failure::new(
                2,
                "RUNNER_TEMP must name an existing absolute directory.",
            ));
        }
        let image = text(&environment, "PRODUCTION_IMAGE").to_owned();
        let tag = !image.is_empty()
            && image.len() <= 256
            && image.as_bytes()[0].is_ascii_alphanumeric()
            && image
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-/:".contains(&byte))
            && image.contains(':')
            && !image.starts_with("sha256:");
        if if verify { !digest(&image) } else { !tag } {
            return Err(Failure::new(
                2,
                if verify {
                    "Verification requires an immutable image ID."
                } else {
                    "Production build requires a literal image tag."
                },
            ));
        }
        let cache_hit = text(&environment, "CACHE_PRIMARY_HIT") == "true";
        let local_arm = local && architecture == "aarch64";
        Ok(Self {
            root: root.into(),
            environment,
            temporary,
            revision,
            image,
            local_arm,
            cache_hit,
        })
    }
}
