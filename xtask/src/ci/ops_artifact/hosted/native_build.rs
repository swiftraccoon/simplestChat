//! Authenticated upstream native builds. Imported sources stay immutable;
//! selected generated outputs are copied into the immutable consumer kit.

use super::{
    catalog::{Catalog, Format, MUSL_COMMIT},
    failure,
    files::{self, Tree},
    kit,
    plan::Plan,
    progress::{Checkpoint, Edge, Progress},
    tools::{InvocationSpec, Role, Tools},
};
use crate::{
    ci::{
        command::Command,
        ops_artifact::namespace::{self, Held},
        process::Executor,
    },
    tooling::{Environment, Result},
};
use object::{
    LittleEndian,
    read::{
        archive::ArchiveFile,
        elf::{ElfFile64, FileHeader},
    },
};
use rustix::fs::OFlags;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs::Metadata,
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const OUTPUT_MAX: u64 = 128 * 1024 * 1024;
const CONFIG_MAX: u64 = 16 * 1024 * 1024;
const HEADER_MAX: u64 = 2 * 1024 * 1024;
const MAX_HEADERS: usize = 4096;
const MUSL_LIBS: [&str; 14] = [
    "crt1.o",
    "Scrt1.o",
    "rcrt1.o",
    "crti.o",
    "crtn.o",
    "libc.a",
    "libm.a",
    "librt.a",
    "libpthread.a",
    "libcrypt.a",
    "libutil.a",
    "libxnet.a",
    "libresolv.a",
    "libdl.a",
];
const DISABLED: [&str; 4] = ["shared", "dso", "module", "engine"];
// Normalize only OpenSSL's generated build-date input; this is not a release date.
const OPENSSL_SOURCE_DATE_EPOCH: u64 = 0;

pub(super) fn openssl_recipe() -> Value {
    json!({"profile":"ops-openssl-build-v1","sourceDateEpoch":OPENSSL_SOURCE_DATE_EPOCH})
}

fn fence(plan: &Plan, end: Instant) -> Result<()> {
    plan.check()?;
    if plan.now()? >= end {
        Err(failure(124))
    } else {
        Ok(())
    }
}

pub(super) fn environment(root: &Path, work: &Path, musl: bool) -> Result<Environment> {
    let mut env = kit::environment(root)?;
    let bins = root.join("host-tools/sysroot/usr/bin");
    let paths = [root.join("host-tools/bin"), bins.clone()];
    env.insert(
        "PATH".into(),
        std::env::join_paths(paths).map_err(|_| failure(1))?,
    );
    let cc = root.join("host-tools/bin/cc");
    env.insert("REALGCC".into(), cc.clone().into_os_string());
    env.insert(
        "CC".into(),
        if musl {
            cc
        } else {
            work.join("musl-prefix/bin/musl-gcc")
        }
        .into_os_string(),
    );
    for (key, name) in [("AR", "ar"), ("RANLIB", "ranlib"), ("NM", "nm")] {
        env.insert(key.into(), bins.join(name).into_os_string());
    }
    for key in ["SHELL", "CONFIG_SHELL"] {
        env.insert(key.into(), bins.join("dash").into_os_string());
    }
    let perl = [
        root.join("host-tools/sysroot/usr/share/perl/5.38"),
        root.join("host-tools/sysroot/usr/lib/x86_64-linux-gnu/perl/5.38"),
        root.join("host-tools/sysroot/usr/lib/x86_64-linux-gnu/perl-base"),
    ];
    env.insert(
        "PERL5LIB".into(),
        std::env::join_paths(perl).map_err(|_| failure(1))?,
    );
    env.insert("HOME".into(), work.join("build-home").into_os_string());
    env.insert("TMPDIR".into(), work.join("build-tmp").into_os_string());
    if !musl {
        env.insert(
            "SOURCE_DATE_EPOCH".into(),
            OPENSSL_SOURCE_DATE_EPOCH.to_string().into(),
        );
    }
    Ok(env)
}

pub(super) struct Step {
    pub role: Role,
    pub directory: PathBuf,
    pub args: Vec<OsString>,
    pub maximum: Duration,
    pub musl: bool,
}
pub(super) fn arguments(work: &Path, root: &Path) -> Vec<Step> {
    let musl = work.join("musl-prefix");
    let openssl = work.join("openssl-prefix");
    let mut linux_include = OsString::from("-I");
    linux_include.push(root.join(kit::LINUX_INCLUDE).as_os_str());
    let shell = format!(
        "SHELL={}",
        root.join("host-tools/sysroot/usr/bin/dash").display()
    );
    vec![
        (
            Role::NativeShell,
            work.join("build-musl"),
            vec![
                work.join("musl/configure").into_os_string(),
                format!("--prefix={}", musl.display()).into(),
                "--target=x86_64-linux-musl".into(),
                "--disable-shared".into(),
                "--enable-wrapper=gcc".into(),
            ],
            Duration::from_secs(60),
            true,
        ),
        (
            Role::NativeMake,
            work.join("build-musl"),
            vec!["-s".into(), "-j2".into(), shell.clone().into()],
            Duration::from_secs(240),
            true,
        ),
        (
            Role::NativeMake,
            work.join("build-musl"),
            vec![
                "-s".into(),
                "-j2".into(),
                shell.clone().into(),
                "install".into(),
            ],
            Duration::from_secs(45),
            true,
        ),
        (
            Role::NativePerl,
            work.join("build-openssl"),
            vec![
                work.join("openssl/Configure").into_os_string(),
                "linux-x86_64".into(),
                format!("--prefix={}", openssl.display()).into(),
                "--libdir=lib".into(),
                linux_include,
                "no-shared".into(),
                "no-dso".into(),
                "no-module".into(),
                "no-tests".into(),
            ],
            Duration::from_secs(60),
            false,
        ),
        (
            Role::NativeMake,
            work.join("build-openssl"),
            vec![
                "-s".into(),
                "-j2".into(),
                shell.clone().into(),
                "build_sw".into(),
            ],
            Duration::from_secs(300),
            false,
        ),
        (
            Role::NativeMake,
            work.join("build-openssl"),
            vec!["-s".into(), "-j2".into(), shell.into(), "install_sw".into()],
            Duration::from_secs(60),
            false,
        ),
    ]
    .into_iter()
    .map(|(role, directory, args, maximum, musl)| Step {
        role,
        directory,
        args,
        maximum,
        musl,
    })
    .collect()
}

pub(super) struct NativeBuild<'a> {
    pub catalog: &'a Catalog,
    pub work: &'a Tree,
    pub tools: &'a Tools,
    pub plan: &'a Plan,
    pub end: Instant,
}
impl NativeBuild<'_> {
    async fn run<E: Executor>(
        &self,
        downloads: &mut Tree,
        executor: &mut E,
        next: &mut usize,
        kit: &Tree,
        step: Step,
    ) -> Result<()> {
        let Step {
            role,
            directory,
            args,
            maximum,
            musl,
        } = step;
        executor.ready().await?;
        fence(self.plan, self.end)?;
        self.work.check_build_sources()?;
        kit.check()?;
        let (body, latest) = self.plan.body(self.end, maximum)?;
        let proof = self.tools.invocation(InvocationSpec {
            role,
            args: args.clone(),
            directory,
            environment: environment(&kit.run.path, &self.work.run.path, musl)?,
            body,
            stdout_maximum: 2 * 1024 * 1024,
            input: None,
            clock: self.plan.clock.clone(),
            latest,
        })?;
        let following = next.checked_add(1).ok_or_else(|| failure(1))?;
        let (logs, names, permits) = downloads.capture(*next, 2 * 1024 * 1024).await?;
        *next = following;
        let mut command = Command::capture(role.program(), &[]);
        command.arguments = args;
        command.capture = None;
        command.separate_logs = Some(logs);
        command.hosted_ops = Some(proof.clone());
        command.timeout = body;
        command.grace = Duration::from_secs(5);
        let ran = executor.run(command).await.map(|_| ());
        drop(permits);
        // Every known writer and immutable input is settled even after a child
        // refusal. A later successful check never replaces the first failure.
        let captured = proof.captures().and_then(|seals| {
            downloads.captured(&names, &seals, &mut || fence(self.plan, self.end))
        });
        let sources = self.work.check_build_sources();
        let kit = kit.check();
        let ready = executor.ready().await;
        let live = fence(self.plan, self.end);
        ran.and(captured).and(sources).and(kit).and(ready).and(live)
    }
}

fn close(owners: Vec<Held>) -> Result<()> {
    let mut result = Ok(());
    for owner in owners.into_iter().rev() {
        result = result.and(owner.finish());
    }
    result
}

struct BuildDirectory {
    name: &'static str,
    owner: Held,
    stamp: Metadata,
}
fn check_build_directories(work: &Tree, directories: &[BuildDirectory]) -> Result<()> {
    let generated = Generated { tree: work };
    for original in directories {
        let visible = generated.directories(original.name)?;
        let checked = (|| {
            let observed = visible.last().ok_or_else(|| failure(1))?;
            let current = original.owner.file().metadata().map_err(|_| failure(1))?;
            if !namespace::binding(&original.stamp, &observed.1)
                || !namespace::binding(&original.stamp, &current)
            {
                return Err(failure(1));
            }
            Ok(())
        })();
        checked.and(close(visible.into_iter().map(|p| p.0).collect()))?;
    }
    Ok(())
}

/// Generated outputs have ordinary upstream modes; immutable imports still use
/// Tree's private modes. All paths are reopened relative to the original root.
pub(super) struct Generated<'a> {
    pub tree: &'a Tree,
}
#[derive(Clone, Copy)]
enum OutputKind {
    Data,
    Configuration,
}
impl Generated<'_> {
    fn directories(&self, name: &str) -> Result<Vec<(Held, Metadata)>> {
        if !files::safe(name) {
            return Err(failure(1));
        }
        self.tree.run.check()?;
        let mut owners: Vec<(Held, Metadata)> = Vec::new();
        let result = (|| {
            for part in name.split('/') {
                let parent = owners.last().map_or(self.tree.run.file(), |p| p.0.file());
                let owner = Held::open(
                    parent,
                    OsStr::new(part),
                    namespace::FLAGS | OFlags::DIRECTORY,
                    self.tree.run.budget(),
                )?;
                let observed = owner.file().metadata().map_err(|_| failure(1));
                let observed = match observed {
                    Ok(stamp) => stamp,
                    Err(error) => {
                        let _ = owner.finish();
                        return Err(error);
                    }
                };
                owners.push((owner, observed));
                let stamp = &owners.last().unwrap().1;
                if !stamp.is_dir()
                    || stamp.uid() != rustix::process::getuid().as_raw()
                    || stamp.mode() & 0o7022 != 0
                {
                    return Err(failure(1));
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = close(owners.into_iter().map(|v| v.0).collect());
            return Err(error);
        }
        Ok(owners)
    }
    fn check_directories(&self, name: &str, owners: &[(Held, Metadata)]) -> Result<()> {
        let visible = self.directories(name)?;
        let checked = if visible.len() == owners.len()
            && owners
                .iter()
                .zip(&visible)
                .all(|((held, before), (_, after))| {
                    namespace::binding(before, after)
                        && held
                            .file()
                            .metadata()
                            .is_ok_and(|now| namespace::binding(before, &now))
                }) {
            Ok(())
        } else {
            Err(failure(1))
        };
        checked.and(close(visible.into_iter().map(|v| v.0).collect()))
    }
    pub(super) fn data(
        &self,
        name: &str,
        maximum: u64,
        checkpoint: &mut impl FnMut() -> Result<()>,
    ) -> Result<Vec<u8>> {
        self.read(name, maximum, OutputKind::Data, checkpoint)
    }
    /// Configure makes this fixed metadata script executable with 0755 & ~umask.
    /// Read it passively under its own role; headers/libraries retain data modes.
    pub(super) fn configuration(
        &self,
        checkpoint: &mut impl FnMut() -> Result<()>,
    ) -> Result<Vec<u8>> {
        self.read(
            "build-openssl/configdata.pm",
            CONFIG_MAX,
            OutputKind::Configuration,
            checkpoint,
        )
    }
    fn read(
        &self,
        name: &str,
        maximum: u64,
        kind: OutputKind,
        checkpoint: &mut impl FnMut() -> Result<()>,
    ) -> Result<Vec<u8>> {
        if !files::safe(name) {
            return Err(failure(1));
        }
        let (parent_name, leaf) = name.rsplit_once('/').ok_or_else(|| failure(1))?;
        let parents = self.directories(parent_name)?;
        let parent = parents.last().unwrap().0.file();
        let result = (|| {
            let file = Held::open(
                parent,
                OsStr::new(leaf),
                namespace::FLAGS,
                self.tree.run.budget(),
            )?;
            let result = (|| {
                let before = file.file().metadata().map_err(|_| failure(1))?;
                let valid_mode = match kind {
                    OutputKind::Data => matches!(before.mode() & 0o7777, 0o600 | 0o644 | 0o444),
                    OutputKind::Configuration => matches!(before.mode() & 0o7777, 0o700 | 0o755),
                };
                if !before.is_file()
                    || before.nlink() != 1
                    || before.uid() != rustix::process::getuid().as_raw()
                    || !valid_mode
                    || before.len() > maximum
                {
                    return Err(failure(1));
                }
                let mut bytes = Vec::new();
                let mut reader = file.file();
                let mut block = [0; 65536];
                loop {
                    checkpoint()?;
                    let n = reader.read(&mut block).map_err(|_| failure(1))?;
                    if n == 0 {
                        break;
                    }
                    if bytes.len() as u64 + n as u64 > maximum {
                        return Err(failure(1));
                    }
                    bytes.extend_from_slice(&block[..n]);
                }
                if bytes.len() as u64 != before.len()
                    || !namespace::same(&before, &file.file().metadata().map_err(|_| failure(1))?)
                {
                    return Err(failure(1));
                }
                let visible = Held::open(
                    parent,
                    OsStr::new(leaf),
                    namespace::FLAGS,
                    self.tree.run.budget(),
                )?;
                let matched = visible
                    .file()
                    .metadata()
                    .map_err(|_| failure(1))
                    .and_then(|now| {
                        if namespace::same(&before, &now) {
                            Ok(())
                        } else {
                            Err(failure(1))
                        }
                    });
                matched.and(visible.finish())?;
                self.check_directories(parent_name, &parents)?;
                Ok(bytes)
            })();
            let closed = file.finish();
            result.and_then(|bytes| closed.map(|()| bytes))
        })();
        let closed = close(parents.into_iter().map(|p| p.0).collect());
        result.and_then(|bytes| closed.map(|()| bytes))
    }
    pub(super) fn headers(
        &self,
        name: &str,
        checkpoint: &mut impl FnMut() -> Result<()>,
    ) -> Result<Vec<String>> {
        let owners = self.directories(name)?;
        let directory = owners.last().unwrap().0.file();
        let result = (|| {
            checkpoint()?;
            let _permit = self.tree.run.budget().permit()?;
            let cursor = rustix::fs::Dir::read_from(directory).map_err(|_| failure(1))?;
            let mut entries = BTreeSet::new();
            for entry in cursor {
                checkpoint()?;
                let entry = entry.map_err(|_| failure(1))?;
                let leaf = entry.file_name().to_str().map_err(|_| failure(1))?;
                if matches!(leaf, "." | "..") {
                    continue;
                }
                if entries.len() >= MAX_HEADERS || !entries.insert(leaf.to_owned()) {
                    return Err(failure(1));
                }
            }
            let mut paths = Vec::new();
            for leaf in entries {
                let owner = Held::open(
                    directory,
                    OsStr::new(&leaf),
                    namespace::FLAGS,
                    self.tree.run.budget(),
                )?;
                let stamp = owner.file().metadata().map_err(|_| failure(1));
                let closed = owner.finish();
                let stamp = stamp?;
                closed?;
                let child = format!("{name}/{leaf}");
                if stamp.is_dir() {
                    paths.extend(self.headers(&child, checkpoint)?);
                } else if stamp.is_file() && leaf.ends_with(".h") {
                    paths.push(child);
                } else {
                    return Err(failure(1));
                }
                if paths.len() > MAX_HEADERS {
                    return Err(failure(1));
                }
            }
            self.check_directories(name, &owners)?;
            Ok(paths)
        })();
        let closed = close(owners.into_iter().map(|p| p.0).collect());
        result.and_then(|paths| closed.map(|()| paths))
    }
}

fn scalar(line: &str) -> Result<(String, String)> {
    let rest = line.strip_prefix("    \"").ok_or_else(|| failure(1))?;
    let (key, rest) = rest.split_once("\" => \"").ok_or_else(|| failure(1))?;
    let value = rest
        .strip_suffix("\",")
        .or_else(|| rest.strip_suffix('"'))
        .ok_or_else(|| failure(1))?;
    if key.is_empty()
        || !key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'+'))
        || value
            .bytes()
            .any(|b| matches!(b, 0..=31 | 127 | b'\\' | b'"' | b'$' | b'@' | b'\''))
    {
        return Err(failure(1));
    }
    Ok((key.to_owned(), value.to_owned()))
}
fn section<'a>(text: &'a str, name: &str) -> Result<&'a str> {
    let marker = format!("our %{name} = (\n");
    let (_, rest) = text.split_once(&marker).ok_or_else(|| failure(1))?;
    if rest.contains(&marker) {
        return Err(failure(1));
    }
    rest.split_once("\n);")
        .map(|p| p.0)
        .ok_or_else(|| failure(1))
}
pub(super) fn config(bytes: &[u8], prefix: &Path) -> Result<Value> {
    if bytes.is_empty() || bytes.len() as u64 > CONFIG_MAX {
        return Err(failure(1));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| failure(1))?;
    let mut disabled = BTreeMap::new();
    for line in section(text, "disabled")?.lines() {
        if line.is_empty() {
            continue;
        }
        let (key, value) = scalar(line)?;
        if disabled.insert(key, value).is_some() {
            return Err(failure(1));
        }
    }
    for key in DISABLED.into_iter().chain(["tests"]) {
        // Pinned OpenSSL 4.1 removed engine support. Its deprecated no-engine
        // option leaves this actual default marker rather than setting option.
        let reason = if key == "engine" { "default" } else { "option" };
        if disabled.get(key).map(String::as_str) != Some(reason) {
            return Err(failure(1));
        }
    }
    let mut selected = BTreeMap::new();
    for line in section(text, "config")?.lines() {
        for key in ["target", "version", "prefix", "libdir"] {
            if line.starts_with(&format!("    \"{key}\" => ")) {
                let (key, value) = scalar(line)?;
                if selected.insert(key, value).is_some() {
                    return Err(failure(1));
                }
            }
        }
    }
    for (key, value) in [
        ("target", "linux-x86_64"),
        ("version", "4.1.0"),
        ("libdir", "lib"),
        ("prefix", prefix.to_str().ok_or_else(|| failure(1))?),
    ] {
        if selected.get(key).map(String::as_str) != Some(value) {
            return Err(failure(1));
        }
    }
    Ok(json!({"shared":true,"dso":true,"module":true,"engine":true}))
}

pub(super) fn elf(bytes: &[u8]) -> Result<()> {
    let file = ElfFile64::<LittleEndian>::parse(bytes).map_err(|_| failure(1))?;
    let header = file.elf_header();
    if header.e_type(LittleEndian) != object::elf::ET_REL
        || header.e_machine(LittleEndian) != object::elf::EM_X86_64
        || !header.is_class_64()
        || !header.is_little_endian()
    {
        return Err(failure(1));
    }
    Ok(())
}
pub(super) fn archive(bytes: &[u8], empty: bool) -> Result<usize> {
    let archive = ArchiveFile::parse(bytes).map_err(|_| failure(1))?;
    if archive.is_thin() {
        return Err(failure(1));
    }
    let mut count = 0;
    for member in archive.members() {
        let member = member.map_err(|_| failure(1))?;
        if count >= 16384 || member.is_thin() || member.name().is_empty() {
            return Err(failure(1));
        }
        elf(member.data(bytes).map_err(|_| failure(1))?)?;
        count += 1;
    }
    if empty != (count == 0) {
        return Err(failure(1));
    }
    Ok(count)
}

fn framed(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}
pub(super) fn inventory(kit: &Tree) -> Result<Value> {
    let mut hash = Sha256::new();
    framed(&mut hash, b"ops-native-output-inventory-v1");
    let mut count = 0_u64;
    let mut bytes = 0_u64;
    for (name, record) in kit
        .records
        .iter()
        .filter(|(name, _)| name.starts_with("native/"))
    {
        if record.stamp.mode() & 0o7777 != 0o600 {
            return Err(failure(1));
        }
        framed(&mut hash, name.as_bytes());
        framed(&mut hash, b"0600");
        framed(&mut hash, &record.bytes.to_be_bytes());
        framed(&mut hash, record.sha256.as_bytes());
        count += 1;
        bytes = bytes.checked_add(record.bytes).ok_or_else(|| failure(1))?;
    }
    if count == 0 {
        return Err(failure(1));
    }
    Ok(json!({
        "algorithm":"sha256-length-framed-v1",
        "sha256":super::super::hex(&hash.finalize()),
        "files":count,"bytes":bytes,"mode":"0600"
    }))
}

fn import_outputs(
    work: &Tree,
    kit: &mut Tree,
    plan: &Plan,
    end: Instant,
    catalog: &Catalog,
) -> Result<Value> {
    let generated = Generated { tree: work };
    let mut checkpoint = || fence(plan, end);
    let configdata = generated.configuration(&mut checkpoint)?;
    let disabled = config(&configdata, &work.run.path.join("openssl-prefix"))?;
    let stock_specs = generated.data("musl-prefix/lib/musl-gcc.specs", 65536, &mut checkpoint)?;
    if !std::str::from_utf8(&stock_specs)
        .map_err(|_| failure(1))?
        .contains(
            &work
                .run
                .path
                .join("musl-prefix")
                .to_string_lossy()
                .to_string(),
        )
    {
        return Err(failure(1));
    }
    for (prefix, destination) in [
        ("musl-prefix/include", "native/musl/include"),
        ("openssl-prefix/include", "native/openssl/include"),
    ] {
        let paths = generated.headers(prefix, &mut checkpoint)?;
        if paths.is_empty() {
            return Err(failure(1));
        }
        for path in paths {
            let data = generated.data(&path, HEADER_MAX, &mut checkpoint)?;
            let suffix = path.strip_prefix(prefix).ok_or_else(|| failure(1))?;
            kit.stream(
                &format!("{destination}{suffix}"),
                data.len() as u64,
                false,
                &mut data.as_slice(),
                &mut checkpoint,
            )?;
        }
    }
    let mut libraries = BTreeMap::new();
    for (prefix, destination, names) in [
        ("musl-prefix/lib", "native/musl/lib", MUSL_LIBS.as_slice()),
        (
            "openssl-prefix/lib",
            "native/openssl/lib",
            ["libssl.a", "libcrypto.a"].as_slice(),
        ),
    ] {
        for &name in names {
            let data = generated.data(&format!("{prefix}/{name}"), OUTPUT_MAX, &mut checkpoint)?;
            let members = if name.ends_with(".o") {
                elf(&data)?;
                1
            } else {
                archive(
                    &data,
                    name.starts_with("lib")
                        && !matches!(name, "libc.a" | "libssl.a" | "libcrypto.a"),
                )?
            };
            let target = format!("{destination}/{name}");
            kit.stream(
                &target,
                data.len() as u64,
                false,
                &mut data.as_slice(),
                &mut checkpoint,
            )?;
            let format = if name.ends_with(".o") {
                "ELF64LE-ET_REL-EM_X86_64"
            } else {
                "ar-ELF64LE-ET_REL-EM_X86_64"
            };
            libraries.insert(
                target,
                json!({
                    "sha256":super::super::sha(&data),"bytes":data.len(),
                    "format":format,"objects":members
                }),
            );
        }
    }
    let mut license_notices = BTreeMap::new();
    for (source, target) in [
        ("musl/COPYRIGHT", "native/musl/COPYRIGHT"),
        ("openssl/LICENSE.txt", "native/openssl/LICENSE.txt"),
    ] {
        let data = work.read(source, 65536, &mut checkpoint)?;
        let component = source.split('/').next().ok_or_else(|| failure(1))?;
        license_notices.insert(
            component,
            json!({
                "sha256":super::super::sha(&data),"bytes":data.len()
            }),
        );
        kit.stream(
            target,
            data.len() as u64,
            false,
            &mut data.as_slice(),
            &mut checkpoint,
        )?;
    }
    let pins = catalog
        .artifacts
        .iter()
        .filter(|a| a.format == Format::NativeGzip)
        .map(|a| {
            (
                a.name.clone(),
                json!({"sha256":a.sha256,"bytes":a.bytes,"version":a.version}),
            )
        })
        .collect::<BTreeMap<_, _>>();
    if pins.len() != 2 {
        return Err(failure(1));
    }
    Ok(json!({
        "schemaVersion":1,"scope":"OpsMuslStaticNativeBuild",
        "target":"x86_64-unknown-linux-musl","muslSourceCommit":MUSL_COMMIT,
        "opensslVersion":"4.1.0","opensslBuildRecipe":openssl_recipe(),
        "sourceArchivePins":pins,
        "configdataSha256":super::super::sha(&configdata),"disabled":disabled,
        "stockMuslSpecsSha256":super::super::sha(&stock_specs),
        "outputInventory":inventory(kit)?,"libraries":libraries,
        "licenseNotices":license_notices
    }))
}

pub(super) async fn build<E: Executor>(
    request: NativeBuild<'_>,
    kit: &mut Tree,
    downloads: &mut Tree,
    executor: &mut E,
    next: &mut usize,
    progress: &mut Progress<'_>,
) -> Result<Value> {
    let NativeBuild {
        catalog,
        work,
        tools: _,
        plan,
        end,
    } = &request;
    let end = *end;
    fence(plan, end)?;
    work.check_build_sources()?;
    kit.check()?;
    let mut directories = Vec::new();
    let result = async {
        for name in ["build-musl", "build-openssl", "build-home", "build-tmp"] {
            let owner = work.run.directory(name)?;
            let stamp = match owner.file().metadata() {
                Ok(stamp) => stamp,
                Err(_) => {
                    let _ = owner.finish();
                    return Err(failure(1));
                }
            };
            directories.push(BuildDirectory { name, owner, stamp });
        }
        for (index, step) in arguments(&work.run.path, &kit.run.path)
            .into_iter()
            .enumerate()
        {
            progress.kit(Checkpoint::NativeCommand, Edge::Begin, index + 1, 6);
            check_build_directories(work, &directories)?;
            let ran = request.run(downloads, executor, next, kit, step).await;
            let checked = check_build_directories(work, &directories);
            ran.and(checked)?;
            progress.kit(Checkpoint::NativeCommand, Edge::Complete, index + 1, 6);
        }
        progress.kit(Checkpoint::NativeOutputs, Edge::Begin, 1, 1);
        let observation = import_outputs(work, kit, plan, end, catalog)?;
        progress.kit(Checkpoint::NativeOutputs, Edge::Complete, 1, 1);
        executor.ready().await?;
        fence(plan, end)?;
        Ok(observation)
    }
    .await;
    let sources = work.check_build_sources();
    let sealed = kit.check();
    let checked = check_build_directories(work, &directories);
    let closed = close(directories.into_iter().map(|p| p.owner).collect());
    let ready = executor.ready().await;
    let live = fence(plan, end);
    result.and_then(|value| {
        sources
            .and(sealed)
            .and(checked)
            .and(closed)
            .and(ready)
            .and(live)
            .map(|()| value)
    })
}
