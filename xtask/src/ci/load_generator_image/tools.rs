//! Selected caller-trusted executable bindings; no toolchain authentication claim.

use super::plan;
use crate::{
    ci::{
        command::Program,
        release_checks::files::{self, Executable},
    },
    tooling::{Environment, Failure, Result},
};
use std::{ffi::OsStr, path::Path, sync::Arc};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ci) enum Role {
    Git,
    Docker,
}
/// The manual owner alone creates these closed invocations. Debug never exposes
/// selected paths or caller environment data.
pub(in crate::ci) struct Invocation {
    role: Role,
    executable: Arc<Executable>,
}
impl std::fmt::Debug for Invocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SelectedLoadImageTool")
    }
}
impl Invocation {
    pub(in crate::ci) fn validate(&self, program: Program) -> Result<&Path> {
        if !matches!(
            (self.role, program),
            (Role::Git, Program::Git) | (Role::Docker, Program::Docker)
        ) {
            return Err(plan::failure());
        }
        self.executable.validate().map_err(|_| plan::failure())
    }
}
pub(in crate::ci) struct Tools {
    git: Arc<Executable>,
    docker: Arc<Executable>,
    zstd: Arc<Executable>,
}
impl Tools {
    pub fn select(root: &Path, environment: &Environment) -> Result<Self> {
        let git = files::select(root, environment, OsStr::new("git"))
            .map_err(|error| Failure::new(error.code, "Load image tool selection failed."))?;
        let docker = match files::select(root, environment, OsStr::new("docker")) {
            Ok(value) => value,
            Err(error) => {
                let _ = git.close();
                return Err(Failure::new(
                    error.code,
                    "Load image tool selection failed.",
                ));
            }
        };
        let zstd = match files::select(root, environment, OsStr::new("zstd")) {
            Ok(value) => value,
            Err(error) => {
                let _ = git.close();
                let _ = docker.close();
                return Err(Failure::new(
                    error.code,
                    "Load image tool selection failed.",
                ));
            }
        };
        Ok(Self {
            git: Arc::new(git),
            docker: Arc::new(docker),
            zstd: Arc::new(zstd),
        })
    }
    pub fn invocation(&self, program: Program) -> Option<Arc<Invocation>> {
        let (role, executable) = match program {
            Program::Git => (Role::Git, &self.git),
            Program::Docker => (Role::Docker, &self.docker),
            _ => return None,
        };
        Some(Arc::new(Invocation {
            role,
            executable: executable.clone(),
        }))
    }
    pub fn zstd(&self) -> Arc<Executable> {
        self.zstd.clone()
    }
    pub fn finish(self) -> Result<()> {
        let mut result = Ok(());
        for selected in [self.git, self.docker, self.zstd] {
            let valid = selected.validate().map(|_| ()).map_err(|_| plan::failure());
            let closed = match Arc::try_unwrap(selected) {
                Ok(value) => value.close().map_err(|_| plan::failure()),
                Err(_) => Err(plan::failure()),
            };
            result = result.and(valid).and(closed);
        }
        result
    }
}
