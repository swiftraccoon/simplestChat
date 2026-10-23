//! Fixed literal recipe. It executes from the fresh run parent, not source/;
//! only its generated Cargo configuration is in the cwd ancestor search.

use super::{
    HOST, TARGET, failure, io,
    namespace::{self, Budget, Chain, Held, OwnedRun},
};
use crate::tooling::{Environment, Result};
use rustix::fs::{AtFlags, OFlags};
use std::{
    ffi::{OsStr, OsString},
    fs::Metadata,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) const CONFIG: &[u8] = b"[source.crates-io]\nreplace-with = \"ops-producer-registry\"\n\
[source.ops-producer-registry]\ndirectory = \"kit/registry\"\n[net]\noffline = true\n";
pub(super) const FLAGS: &str = concat!(
    "-C\u{1f}target-feature=+crt-static\u{1f}-C\u{1f}target-cpu=x86-64\u{1f}",
    "-C\u{1f}link-self-contained=yes\u{1f}-C\u{1f}linker-flavor=ld.lld",
);
const CARGO: &str = "toolchain/bin/cargo";
const RUSTC: &str = "toolchain/bin/rustc";
const LLD: &str = "toolchain/lib/rustlib/x86_64-unknown-linux-gnu/bin/rust-lld";
const CC: &str = "host-tools/bin/cc";

/// Only these direct execution roles can be passed to Native. Linker and host
/// tools are pinned material inputs, not independently supervised descendants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ci) enum Role {
    Cargo,
    Rustc,
}

struct Root {
    chain: Chain,
    budget: Budget,
    directory: PathBuf,
    environment: Environment,
}
pub(in crate::ci) struct Tool {
    root: Arc<Root>,
    relative: &'static str,
    stamps: Vec<Metadata>,
    path: PathBuf,
    role: Option<Role>,
}
impl std::fmt::Debug for Tool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SelectedOpsProducerTool")
    }
}
impl Tool {
    fn open(
        root: Arc<Root>,
        relative: &'static str,
        role: Option<Role>,
        stamps: Vec<Metadata>,
    ) -> Result<Self> {
        let path = root.chain.path.join(relative);
        let value = Self {
            root,
            relative,
            stamps,
            path,
            role,
        };
        value.walk(true)?;
        Ok(value)
    }
    fn walk(&self, checking: bool) -> Result<Vec<Metadata>> {
        self.root.chain.verify()?;
        let mut owners: Vec<Held> = Vec::new();
        let mut stamps = Vec::new();
        let parts: Vec<_> = self.relative.split('/').collect();
        let observed = (|| {
            for (i, part) in parts.iter().enumerate() {
                let parent = owners.last().map_or(self.root.chain.file(), Held::file);
                let flags = namespace::FLAGS
                    | if i + 1 < parts.len() {
                        OFlags::DIRECTORY
                    } else {
                        OFlags::empty()
                    };
                let owner = Held::open(parent, OsStr::new(part), flags, &self.root.budget)?;
                owners.push(owner);
                let metadata = io(owners.last().unwrap().file().metadata())?;
                if checking && !namespace::same(&self.stamps[i], &metadata) {
                    return Err(failure(1));
                }
                if i + 1 == parts.len()
                    && (!metadata.is_file()
                        || metadata.nlink() != 1
                        || metadata.mode() & 0o7777 != 0o700
                        || metadata.len() == 0)
                {
                    return Err(failure(1));
                }
                stamps.push(metadata);
            }
            Ok(())
        })();
        let mut closed = true;
        for owner in owners.into_iter().rev() {
            closed &= owner.finish().is_ok();
        }
        observed?;
        if !closed {
            return Err(failure(1));
        }
        self.root.chain.verify()?;
        Ok(stamps)
    }
    pub fn validate(&self, role: Role) -> Result<&Path> {
        if self.role != Some(role) {
            return Err(failure(1));
        }
        self.walk(true)?;
        Ok(&self.path)
    }
    pub(in crate::ci) fn context(&self, role: Role) -> Result<(&Path, &Environment)> {
        self.validate(role)?;
        Ok((&self.root.directory, &self.root.environment))
    }
}
pub(super) struct Tools {
    root: Arc<Root>,
    pub cargo: Arc<Tool>,
    pub rustc: Arc<Tool>,
    lld: Tool,
    cc: Tool,
    pub identities: serde_json::Value,
}
impl Tools {
    pub fn open(run: &OwnedRun, materials: &super::materials::Materials) -> Result<Self> {
        let host_cc = materials.tool_digest(CC)?;
        let mut environment = Recipe::environment(run)?;
        if host_cc["sha256"].as_str() == Some(super::hosted::cc_sha256().as_str()) {
            environment.extend(super::hosted::cc_environment(&run.path)?);
        }
        let root = Arc::new(Root {
            chain: Chain::open(&run.path.join("kit"), run.budget())?,
            budget: run.budget().clone(),
            directory: run.path.clone(),
            environment,
        });
        let admitted = (|| {
            if !namespace::same(materials.kit_root(), &io(root.chain.file().metadata())?) {
                return Err(failure(1));
            }
            Ok(Self {
                identities: serde_json::json!({"cargo":materials.tool_digest(CARGO)?,
                "rustc":materials.tool_digest(RUSTC)?,"rustLld":materials.tool_digest(LLD)?,
                "hostCc":host_cc}),
                cargo: Arc::new(Tool::open(
                    root.clone(),
                    CARGO,
                    Some(Role::Cargo),
                    materials.kit_stamps(CARGO)?,
                )?),
                rustc: Arc::new(Tool::open(
                    root.clone(),
                    RUSTC,
                    Some(Role::Rustc),
                    materials.kit_stamps(RUSTC)?,
                )?),
                lld: Tool::open(root.clone(), LLD, None, materials.kit_stamps(LLD)?)?,
                cc: Tool::open(root.clone(), CC, None, materials.kit_stamps(CC)?)?,
                root: root.clone(),
            })
        })();
        match admitted {
            Ok(value) => Ok(value),
            Err(error) => {
                let _ = Arc::try_unwrap(root)
                    .map_err(|_| failure(1))
                    .and_then(|root| root.chain.finish());
                Err(error)
            }
        }
    }
    pub fn check(&self) -> Result<()> {
        self.cargo.walk(true)?;
        self.rustc.walk(true)?;
        self.lld.walk(true)?;
        self.cc.walk(true)?;
        Ok(())
    }
    pub fn finish(self) -> Result<()> {
        drop(self.cargo);
        drop(self.rustc);
        drop(self.lld);
        drop(self.cc);
        match Arc::try_unwrap(self.root) {
            Ok(root) => root.chain.finish(),
            Err(_) => Err(failure(1)),
        }
    }
}

pub(super) struct Recipe {
    pub environment: Environment,
    directories: Vec<(&'static str, Held, Metadata)>,
    config: Metadata,
}
impl Recipe {
    pub fn create(run: &OwnedRun, tools: &Tools) -> Result<Self> {
        run.check()?;
        tools.check()?;
        let mut directories = Vec::new();
        let admitted = (|| {
            for name in [".cargo", "cargo-home", "home", "tmp", "target"] {
                let owner = run.directory(name)?;
                let stamp = match io(owner.file().metadata()) {
                    Ok(stamp) => stamp,
                    Err(error) => {
                        let _ = owner.finish();
                        return Err(error);
                    }
                };
                directories.push((name, owner, stamp));
            }
            namespace::write(directories[0].1.file(), "config.toml", CONFIG, run.budget())
        })();
        let config = match admitted {
            Ok(config) => config,
            Err(error) => {
                for (_, owner, _) in directories.into_iter().rev() {
                    let _ = owner.finish();
                }
                return Err(error);
            }
        };
        let environment = tools.root.environment.clone();
        Ok(Self {
            environment,
            directories,
            config,
        })
    }
    pub fn environment(run: &OwnedRun) -> Result<Environment> {
        let mut environment = Environment::new();
        for (name, value) in [
            ("CARGO_HOME", run.path.join("cargo-home").into_os_string()),
            ("HOME", run.path.join("home").into_os_string()),
            ("TMPDIR", run.path.join("tmp").into_os_string()),
            (
                "PATH",
                std::env::join_paths([
                    run.path.join("kit/toolchain/bin"),
                    run.path.join("kit/host-tools/bin"),
                ])
                .map_err(|_| failure(1))?,
            ),
            ("RUSTC", run.path.join("kit").join(RUSTC).into_os_string()),
            (
                "CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER",
                run.path.join("kit").join(LLD).into(),
            ),
            (
                "CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER",
                run.path.join("kit").join(CC).into(),
            ),
            ("CC", run.path.join("kit").join(CC).into_os_string()),
            ("CARGO_ENCODED_RUSTFLAGS", FLAGS.into()),
            ("CARGO_INCREMENTAL", "0".into()),
            ("CARGO_TERM_COLOR", "never".into()),
            ("LC_ALL", "C".into()),
            ("TZ", "UTC".into()),
        ] {
            environment.insert(name.into(), value);
        }
        Ok(environment)
    }
    pub fn check(&self, run: &OwnedRun, tools: &Tools) -> Result<()> {
        run.check()?;
        tools.check()?;
        for (name, owner, initial) in &self.directories {
            let visible = Held::open(
                run.file(),
                OsStr::new(name),
                namespace::FLAGS | OFlags::DIRECTORY,
                run.budget(),
            )?;
            let matched = (|| {
                Ok(namespace::binding(initial, &io(owner.file().metadata())?)
                    && namespace::binding(initial, &io(visible.file().metadata())?))
            })();
            let closed = visible.finish();
            let matched = matched.and_then(|matched| closed.map(|()| matched))?;
            if !matched {
                return Err(failure(1));
            }
        }
        for (index, names) in [(0, vec!["config"]), (1, vec!["config", "config.toml"])] {
            for name in names {
                if !matches!(
                    rustix::fs::statat(
                        self.directories[index].1.file(),
                        name,
                        AtFlags::SYMLINK_NOFOLLOW
                    ),
                    Err(rustix::io::Errno::NOENT)
                ) {
                    return Err(failure(1));
                }
            }
        }
        let config = Held::open(
            self.directories[0].1.file(),
            OsStr::new("config.toml"),
            namespace::FLAGS,
            run.budget(),
        )?;
        let result = (|| {
            if !namespace::same(&self.config, &io(config.file().metadata())?) {
                return Err(failure(1));
            }
            let mut bytes = Vec::new();
            io(config
                .file()
                .take(CONFIG.len() as u64 + 1)
                .read_to_end(&mut bytes))?;
            if bytes != CONFIG || !namespace::same(&self.config, &io(config.file().metadata())?) {
                return Err(failure(1));
            }
            Ok(())
        })();
        let closed = config.finish();
        result?;
        closed
    }
    pub fn arguments(run: &OwnedRun, metadata: bool) -> Vec<OsString> {
        let mut args = if metadata {
            vec![
                "metadata",
                "--frozen",
                "--format-version",
                "1",
                "--filter-platform",
                TARGET,
            ]
        } else {
            vec![
                "build",
                "--frozen",
                "--release",
                "-p",
                "simplestchat-ops",
                "--bin",
                "simplestchat-ops",
                "--target",
                TARGET,
            ]
        }
        .into_iter()
        .map(OsString::from)
        .collect::<Vec<_>>();
        args.extend([
            OsString::from("--manifest-path"),
            run.path.join("source/Cargo.toml").into(),
        ]);
        if !metadata {
            args.extend([
                OsString::from("--target-dir"),
                run.path.join("target").into(),
            ]);
        }
        args
    }
    pub fn projection(&self) -> serde_json::Value {
        serde_json::json!({"schemaVersion":1,"profile":"ops-producer-recipe-v1",
            "workingDirectory":"freshRunRoot","manifest":"stagedSource/Cargo.toml",
            "configuration":"generatedDirectorySourceOnly","environmentPolicy":"clearThenSet",
            "environmentNames":(self.environment.keys()
                .map(|k|k.to_string_lossy()).collect::<Vec<_>>()),
            "rustflags":FLAGS,"target":TARGET,"host":HOST,"toolchainVersion":super::VERSION,
            "staticPieExpectation":"pinnedTargetDefaultOnly","candidateExecuted":false})
    }
    pub fn finish(self) -> Result<()> {
        let mut closed = true;
        for (_, owner, _) in self.directories.into_iter().rev() {
            closed &= owner.finish().is_ok();
        }
        if closed { Ok(()) } else { Err(failure(1)) }
    }
}
