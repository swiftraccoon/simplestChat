//! Explicit CI admission and the documented local already-built-image facade.

use crate::{
    ci::runner,
    tooling::{Environment, Failure, Result},
};
use std::{
    ffi::OsStr,
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    Ci,
    Local,
}

pub(super) struct Context {
    pub root: PathBuf,
    pub environment: Environment,
    pub image: String,
    pub artifacts: Option<PathBuf>,
    pub temporary: PathBuf,
}

pub(super) fn image_id(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(container_id)
}

pub(super) fn container_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn safe_path(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().as_encoded_bytes().len() <= 4096
        && !path
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .any(|byte| matches!(byte, 0..=31 | 127))
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}

impl Context {
    pub fn new(root: &Path, environment: Environment, mode: Mode, system: &str) -> Result<Self> {
        if mode == Mode::Ci && !runner::authorized(&environment, system) {
            return Err(Failure::new(
                2,
                "Container smoke requires hosted or disposable Linux CI.",
            ));
        }
        let selected = environment
            .get(OsStr::new("PRODUCTION_IMAGE"))
            .filter(|value| !value.is_empty())
            .map(|value| {
                value
                    .to_str()
                    .ok_or_else(|| Failure::new(2, "Container smoke image selection invalid."))
            })
            .transpose()?
            .unwrap_or("");
        let image = if selected.is_empty() {
            "simplestchat-ci:production"
        } else {
            selected
        };
        if image.len() > 512
            || !image.as_bytes()[0].is_ascii_alphanumeric()
            || !image
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-/:@".contains(&byte))
            || (mode == Mode::Ci && !image_id(image))
        {
            return Err(Failure::new(2, "Container smoke image selection invalid."));
        }
        let selected = environment
            .get(OsStr::new("CONTAINER_TEST_ARTIFACTS"))
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let artifacts = selected.map(|path| {
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        });
        let temporary = environment
            .get(OsStr::new("TMPDIR"))
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| "/tmp".into());
        if !safe_path(root)
            || artifacts.as_deref().is_some_and(|path| !safe_path(path))
            || !safe_path(&temporary)
        {
            return Err(Failure::new(2, "Container smoke artifact path invalid."));
        }
        Ok(Self {
            root: root.into(),
            image: image.into(),
            environment,
            artifacts,
            temporary,
        })
    }

    pub fn frozen_environment(&self) -> Environment {
        let mut value = self.environment.clone();
        for name in [
            "DOCKER_CONTEXT",
            "DOCKER_HOST",
            "DOCKER_TLS_VERIFY",
            "DOCKER_CERT_PATH",
            "DOCKER_API_VERSION",
            "BUILDX_BUILDER",
            "BUILDKIT_HOST",
        ] {
            value.remove(OsStr::new(name));
        }
        value
    }
}
