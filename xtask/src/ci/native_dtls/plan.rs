//! Fixed native regression commands and the reference's clean environment.

use crate::{
    ci::runner,
    tooling::{Environment, Failure, Result},
};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) const LOG_LIMIT: u64 = 64 * 1024 * 1024;
pub(super) const BUDGET: Duration = Duration::from_secs(12 * 60);
pub(super) const RESERVE: Duration = Duration::from_secs(40);
pub(super) const LOGS: [&str; 3] = ["build.log", "dtls.log", "orderly-close.log"];
pub(super) fn failure(code: u8) -> Failure {
    Failure::new(code, "Native DTLS regression check failed.")
}
fn selected(environment: &Environment, name: &str, fallback: &OsStr) -> OsString {
    environment
        .get(OsStr::new(name))
        .filter(|value| !value.is_empty())
        .map_or_else(|| fallback.to_owned(), Clone::clone)
}
pub(super) struct Context {
    pub root: PathBuf,
    pub cwd: PathBuf,
    pub environment: Environment,
    pub temporary: PathBuf,
    pub openssl: PathBuf,
    pub python: OsString,
    pub cc: OsString,
    pub cxx: OsString,
    pub revision: Option<String>,
}
impl Context {
    pub fn new(
        root: &Path,
        cwd: &Path,
        environment: Environment,
        system: &str,
        record: bool,
    ) -> Result<Self> {
        if !matches!(system, "Linux" | "Darwin") || !root.is_absolute() || !cwd.is_absolute() {
            return Err(failure(2));
        }
        let openssl = PathBuf::from(selected(
            &environment,
            "OPENSSL_DIR",
            root.join("target/openssl-4.1.0").as_os_str(),
        ));
        let temporary = PathBuf::from(selected(
            &environment,
            "RUNNER_TEMP",
            &selected(&environment, "TMPDIR", OsStr::new("/tmp")),
        ));
        if !openssl.is_absolute()
            || !openssl.join("lib/pkgconfig/openssl.pc").is_file()
            || !temporary.is_absolute()
            || !temporary.is_dir()
        {
            return Err(failure(2));
        }
        let revision = if record {
            let sha = runner::text(&environment, "GITHUB_SHA");
            if !runner::authorized(&environment, system)
                || sha.len() != 40
                || !sha
                    .bytes()
                    .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
                || runner::text(&environment, "RUNNER_TEMP").is_empty()
            {
                return Err(failure(2));
            }
            Some(sha.to_owned())
        } else {
            None
        };
        Ok(Self {
            root: root.canonicalize().map_err(|_| failure(2))?,
            cwd: cwd.to_owned(),
            temporary: temporary.canonicalize().map_err(|_| failure(2))?,
            openssl,
            python: selected(&environment, "PYTHON", OsStr::new("python3")),
            cc: selected(&environment, "CC", OsStr::new("clang")),
            cxx: selected(&environment, "CXX", OsStr::new("clang++")),
            environment,
            revision,
        })
    }
    pub fn clean_environment(&self, prefix: &Path) -> Environment {
        let mut value = Environment::from([
            (
                "PATH".into(),
                self.environment
                    .get(OsStr::new("PATH"))
                    .cloned()
                    .unwrap_or_default(),
            ),
            ("PYTHON".into(), self.python.clone()),
            ("CC".into(), self.cc.clone()),
            ("CXX".into(), self.cxx.clone()),
            ("PYTHONNOUSERSITE".into(), "1".into()),
            ("PYTHONDONTWRITEBYTECODE".into(), "1".into()),
            ("PYTHONPATH".into(), prefix.join("out/pip_invoke").into()),
            ("PIP_CONFIG_FILE".into(), "/dev/null".into()),
            ("PIP_INDEX_URL".into(), "https://pypi.org/simple".into()),
            (
                "PIP_CONSTRAINT".into(),
                self.root.join("build/pip-constraints.txt").into(),
            ),
            (
                "PKG_CONFIG_PATH".into(),
                self.openssl.join("lib/pkgconfig").into(),
            ),
            ("MEDIASOUP_OUT_DIR".into(), prefix.join("out").into()),
            (
                "MEDIASOUP_INSTALL_DIR".into(),
                prefix.join("install").into(),
            ),
            ("BUILD_DIR".into(), prefix.join("build").into()),
            (
                "MEDIASOUP_TEST_TAGS".into(),
                "[dtls],[media-diagnostics],[bandwidth-constraints]".into(),
            ),
            ("MS_TEST_LOG_LEVEL".into(), "warn".into()),
            ("MS_TEST_LOG_TAGS".into(), "dtls rtp ice score".into()),
        ]);
        // Match env-i even if Cargo's process supplied an ambient PWD.
        value.remove(OsStr::new("PWD"));
        value
    }
}
