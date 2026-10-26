//! Fixed retained programs and one reserved child-group owner per invocation.

use super::{
    Error, Result,
    control::Control,
    files::{self, Directory, Original},
    work::Work,
};
use simplestchat_ops::owned_process::{Group, exited_now, finish_before};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File, Metadata},
    io::Write,
    os::{
        fd::OwnedFd,
        unix::{
            fs::{MetadataExt, PermissionsExt},
            process::{CommandExt, ExitStatusExt},
        },
    },
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const CAPTURE: usize = 2 * 1024 * 1024;
const MAX_COMMANDS: usize = 64;
const TOOLS: &[&str] = &[
    "gh",
    "curl",
    "ssh",
    "git",
    "qemu-system-x86_64",
    "qemu-img",
    "cloud-localds",
    "prlimit",
    "timeout",
    "ssh-keygen",
    "ansible-playbook",
];
struct Tool {
    alias: PathBuf,
    original: Original,
}
pub(super) struct Observed {
    pub status: i32,
    pub stdout: Vec<u8>,
}
pub(super) struct Runner {
    pub root: PathBuf,
    pub output: PathBuf,
    pub work: PathBuf,
    pub env: BTreeMap<OsString, OsString>,
    pub sources: BTreeMap<String, Original>,
    tools: BTreeMap<String, Tool>,
    count: usize,
    guest_health: Option<Arc<AtomicBool>>,
    guest_failure: Option<Arc<Mutex<Option<Error>>>>,
    private_directory: Option<Directory>,
    private_links: BTreeMap<String, Metadata>,
    work_owner: Option<Work>,
}
impl Runner {
    pub fn new(root: PathBuf, output: PathBuf, control: &mut Control) -> Result<Self> {
        control.check()?;
        let local = output.join("ansible-local");
        // Ansible requires a UTF-8 locale even when Python enables UTF-8 mode.
        let locale = if cfg!(target_os = "linux") {
            "C.UTF-8"
        } else {
            "en_US.UTF-8"
        };
        let mut env = BTreeMap::from([
            ("PATH".into(), std::env::var_os("PATH").ok_or(Error::Input)?),
            ("HOME".into(), output.as_os_str().into()),
            ("LANG".into(), locale.into()),
            ("LC_ALL".into(), locale.into()),
            (
                "ANSIBLE_CONFIG".into(),
                root.join("ops/ansible/ansible.cfg").into_os_string(),
            ),
            ("ANSIBLE_LOCAL_TEMP".into(), local.as_os_str().into()),
            ("ANSIBLE_NOCOLOR".into(), "1".into()),
            ("ANSIBLE_STDOUT_CALLBACK".into(), "default".into()),
            ("ANSIBLE_LOAD_CALLBACK_PLUGINS".into(), "0".into()),
        ]);
        if let Some(value) = std::env::var_os("RUNNER_TRACKING_ID") {
            let text = value.to_str().ok_or(Error::Input)?;
            if text.is_empty()
                || text.len() > 128
                || !text
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            {
                return Err(Error::Input);
            }
            env.insert("RUNNER_TRACKING_ID".into(), value);
        }
        let work_owner = Work::create(&output, control)?;
        let work = work_owner.path.clone();
        let mut value = Self {
            root,
            output,
            work,
            env,
            sources: BTreeMap::new(),
            tools: BTreeMap::new(),
            count: 0,
            guest_health: None,
            guest_failure: None,
            private_directory: None,
            private_links: BTreeMap::new(),
            work_owner: Some(work_owner),
        };
        let admitted = (|| {
            fs::create_dir(&local).map_err(|_| Error::Io)?;
            fs::set_permissions(&local, fs::Permissions::from_mode(0o700))
                .map_err(|_| Error::Io)?;
            for name in TOOLS {
                let alias = lookup(name)?;
                let path = fs::canonicalize(&alias).map_err(|_| Error::Io)?;
                let original = Original::system(&path, 320 * 1024 * 1024, control)?;
                value.tools.insert((*name).into(), Tool { alias, original });
            }
            for (name, relative) in [
                ("controller", "target/debug/xtask"),
                (
                    "ops",
                    "target/x86_64-unknown-linux-gnu/release/simplestchat-ops",
                ),
            ] {
                let alias = value.root.join(relative);
                let path = fs::canonicalize(&alias).map_err(|_| Error::Io)?;
                let original = Original::open(&path, 320 * 1024 * 1024, control)?;
                value.tools.insert(name.into(), Tool { alias, original });
            }
            let private = value.work.join("native-tools");
            fs::create_dir(&private).map_err(|_| Error::Io)?;
            fs::set_permissions(&private, fs::Permissions::from_mode(0o700))
                .map_err(|_| Error::Io)?;
            for name in ["gh", "curl", "ssh"] {
                std::os::unix::fs::symlink(&value.tools[name].original.path, private.join(name))
                    .map_err(|_| Error::Io)?;
                let stamp = fs::symlink_metadata(private.join(name)).map_err(|_| Error::Io)?;
                if !stamp.file_type().is_symlink()
                    || stamp.uid() != rustix::process::getuid().as_raw()
                {
                    return Err(Error::Refused);
                }
                value.private_links.insert(name.into(), stamp);
            }
            value.private_directory = Some(Directory::open(&private, control)?);
            value.env.insert(
                "ANSIBLE_SSH_EXECUTABLE".into(),
                value.tools["ssh"].original.path.as_os_str().into(),
            );
            control.check()
        })();
        if let Err(error) = admitted {
            control.record(error);
            let _ = value.finish(control);
            return Err(error);
        }
        Ok(value)
    }
    pub fn set_guest_health(&mut self, value: Arc<AtomicBool>, failure: Arc<Mutex<Option<Error>>>) {
        self.guest_health = Some(value);
        self.guest_failure = Some(failure);
    }
    pub fn source(&self, name: &str, control: &mut Control) -> Result<Vec<u8>> {
        self.sources
            .get(name)
            .ok_or(Error::Input)?
            .data(1024 * 1024, control)
    }
    pub fn tool(&self, name: &str, control: &mut Control) -> Result<OsString> {
        control.check()?;
        self.work_owner
            .as_ref()
            .ok_or(Error::Changed)?
            .check(control)?;
        let tool = self.tools.get(name).ok_or(Error::Input)?;
        if (!matches!(name, "controller" | "ops") && lookup(name)? != tool.alias)
            || fs::canonicalize(&tool.alias).map_err(|_| Error::Io)? != tool.original.path
        {
            return Err(Error::Changed);
        }
        tool.original.metadata(control)?;
        Ok(tool.original.path.as_os_str().into())
    }
    pub fn check(&self, control: &mut Control) -> Result<()> {
        if let Some(error) = self
            .guest_failure
            .as_ref()
            .and_then(|slot| slot.lock().ok().and_then(|value| *value))
        {
            control.record(error);
        }
        control.check()?;
        self.work_owner
            .as_ref()
            .ok_or(Error::Changed)?
            .check(control)?;
        if !control.settling
            && self
                .guest_health
                .as_ref()
                .is_some_and(|value| !value.load(Ordering::SeqCst))
        {
            let observed = self
                .guest_failure
                .as_ref()
                .and_then(|slot| slot.lock().ok().and_then(|value| *value));
            return Err(observed.unwrap_or(Error::Refused));
        }
        self.private_tools(control)?;
        for original in self.sources.values() {
            original.metadata(control)?;
        }
        for name in self.tools.keys() {
            let _ = self.tool(name, control)?;
        }
        control.check()
    }
    pub fn integrity(&self, control: &mut Control) -> Result<()> {
        self.check(control)?;
        for original in self.sources.values() {
            original.check(control)?;
        }
        for tool in self.tools.values() {
            tool.original.check(control)?;
        }
        control.check()
    }
    fn private_tools(&self, control: &mut Control) -> Result<()> {
        control.check()?;
        let directory = self.private_directory.as_ref().ok_or(Error::Changed)?;
        directory.check(control)?;
        let path = self.work.join("native-tools");
        if directory.names(control)? != ["curl", "gh", "ssh"] {
            return Err(Error::Changed);
        }
        for name in ["gh", "curl", "ssh"] {
            link_check(
                self.private_links.get(name).ok_or(Error::Changed)?,
                &path.join(name),
                &self.tools[name].original.path,
                control,
            )?;
        }
        control.check()
    }
    pub async fn credential(
        &mut self,
        control: &mut Control,
        role: &str,
        tool: &str,
        args: Vec<OsString>,
        cap: Duration,
    ) -> Result<Observed> {
        let token = std::env::var_os("GH_TOKEN").ok_or(Error::Input)?;
        if token.is_empty() || token.as_encoded_bytes().len() > 16384 {
            return Err(Error::Input);
        }
        let mut environment = self.env.clone();
        environment.insert("GH_TOKEN".into(), token);
        environment.insert("GH_PROMPT_DISABLED".into(), "1".into());
        environment.insert("GH_NO_UPDATE_NOTIFIER".into(), "1".into());
        environment.insert("GH_NO_EXTENSION_UPDATE_NOTIFIER".into(), "1".into());
        let path = format!("{}:/usr/bin:/bin", self.work.join("native-tools").display());
        environment.insert("PATH".into(), path.into());
        self.execute(
            control,
            Call {
                role,
                tool,
                args,
                cap,
                accepted: &[0],
                input: None,
                environment: Some(environment),
            },
        )
        .await
    }
    pub fn command(&self, tool: &str, args: &[OsString], control: &mut Control) -> Result<Command> {
        self.integrity(control)?;
        let path = self.tool(tool, control)?;
        let mut command = Command::new(path);
        command
            .args(args)
            .env_clear()
            .envs(&self.env)
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0);
        control.check()?;
        Ok(command)
    }
    pub fn log(&mut self, role: &str, suffix: &str, control: &mut Control) -> Result<File> {
        if role.is_empty()
            || role.len() > 64
            || !role
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || !["stdout", "stderr"].contains(&suffix)
        {
            return Err(Error::Input);
        }
        files::private_file(
            &self.output.join(format!("{}-{role}.{suffix}", self.count)),
            control,
        )
    }
    pub async fn run(
        &mut self,
        control: &mut Control,
        role: &str,
        tool: &str,
        args: Vec<OsString>,
        cap: Duration,
        accepted: &[i32],
    ) -> Result<Observed> {
        self.execute(
            control,
            Call {
                role,
                tool,
                args,
                cap,
                accepted,
                input: None,
                environment: None,
            },
        )
        .await
    }
    pub async fn run_input(
        &mut self,
        control: &mut Control,
        role: &str,
        args: Vec<OsString>,
        cap: Duration,
        input: &File,
    ) -> Result<Observed> {
        self.execute(
            control,
            Call {
                role,
                tool: "ssh",
                args,
                cap,
                accepted: &[0, 1, 2, 124, 130, 143],
                input: Some(input),
                environment: None,
            },
        )
        .await
    }
    async fn execute(&mut self, control: &mut Control, call: Call<'_>) -> Result<Observed> {
        let Call {
            role,
            tool,
            args,
            cap,
            accepted,
            input,
            environment,
        } = call;
        self.check(control)?;
        if self.count >= MAX_COMMANDS {
            return Err(Error::Refused);
        }
        self.count += 1;
        let mut logs = Vec::new();
        let mut pipes: Vec<File> = Vec::new();
        let mut group: Option<Group> = None;
        let mut raw = None;
        let mut first = None;
        let mut captured = Vec::new();
        let mut eof = [false, false];
        let allowance = control.remaining(cap + Duration::from_secs(10))?;
        if allowance <= Duration::from_secs(10) {
            return Err(Error::Deadline);
        }
        let end = Instant::now() + allowance;
        let body_end = end - Duration::from_secs(10);
        let observed: Result<()> = async {
            logs.push(self.log(role, "stdout", control)?);
            if Instant::now() >= body_end {
                return Err(Error::Deadline);
            }
            logs.push(self.log(role, "stderr", control)?);
            let mut command = self.command(tool, &args, control)?;
            if let Some(environment) = environment {
                command.env_clear().envs(environment);
            }
            if let Some(input) = input {
                // SDK duplicate close is opaque; retained original closes separately.
                command.stdin(Stdio::from(input.try_clone().map_err(|_| Error::Io)?));
            }
            control.check()?;
            if Instant::now() >= body_end {
                return Err(Error::Deadline);
            }
            let child = command.spawn().map_err(|_| Error::Io)?;
            group = Some(Group::new(child));
            let held = group.as_mut().ok_or(Error::Refused)?;
            for pipe in [
                held.child().stdout.take().map(OwnedFd::from),
                held.child().stderr.take().map(OwnedFd::from),
            ]
            .into_iter()
            .flatten()
            {
                pipes.push(File::from(pipe));
            }
            control.check()?;
            if pipes.len() != 2 {
                return Err(Error::Close);
            }
            for pipe in &pipes {
                let flags = rustix::fs::fcntl_getfl(pipe).map_err(|_| Error::Io)?;
                rustix::fs::fcntl_setfl(pipe, flags | rustix::fs::OFlags::NONBLOCK)
                    .map_err(|_| Error::Io)?;
            }
            let mut counts = [0usize; 2];
            while !eof.iter().all(|value| *value)
                || !exited_now(held.pid()).map_err(|_| Error::Close)?
            {
                self.check(control)?;
                if Instant::now() >= body_end {
                    return Err(Error::Deadline);
                }
                for index in 0..2 {
                    let mut block = [0; 65536];
                    match rustix::io::read(&pipes[index], &mut block) {
                        Ok(0) => eof[index] = true,
                        Ok(read) => {
                            counts[index] =
                                counts[index].checked_add(read).ok_or(Error::Refused)?;
                            if counts[index] > CAPTURE {
                                return Err(Error::Refused);
                            }
                            logs[index]
                                .write_all(&block[..read])
                                .map_err(|_| Error::Io)?;
                            if index == 0 {
                                captured.extend_from_slice(&block[..read]);
                            }
                            control.check()?;
                        }
                        Err(rustix::io::Errno::AGAIN | rustix::io::Errno::INTR) => {}
                        Err(_) => return Err(Error::Io),
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            Ok(())
        }
        .await;
        if let Err(error) = observed {
            first = Some(error);
            control.record(error);
        }
        if let Some(mut held) = group {
            let retirement = end.min(control.end());
            if !finish_before(held.pid(), retirement).await {
                first.get_or_insert(Error::Close);
                control.record(Error::Close);
                control.cleanup_failed = true;
            }
            match held.reap().await {
                Ok(status) => {
                    let value = status
                        .code()
                        .unwrap_or_else(|| -status.signal().unwrap_or(0));
                    raw = Some(value);
                    if !accepted.contains(&value) {
                        first.get_or_insert(Error::Child(value));
                        control.record(Error::Child(value));
                    }
                }
                Err(_) => {
                    first.get_or_insert(Error::Close);
                    control.record(Error::Close);
                    control.cleanup_failed = true;
                }
            }
        }
        for file in pipes.into_iter().chain(logs) {
            if nix::unistd::close(file).is_err() {
                first.get_or_insert(Error::Close);
                control.record(Error::Close);
                control.cleanup_failed = true;
            }
        }
        if let Err(error) = self.integrity(control) {
            first.get_or_insert(error);
        }
        if Instant::now() >= end {
            first.get_or_insert(Error::Deadline);
        }
        if let Some(error) = first {
            return Err(error);
        }
        let status = raw.ok_or(Error::Close)?;
        if !accepted.contains(&status) {
            return Err(Error::Child(status));
        }
        Ok(Observed {
            status,
            stdout: captured,
        })
    }
    pub fn finish(&mut self, control: &mut Control) -> Result<()> {
        let mut first = None;
        for original in self
            .sources
            .values_mut()
            .chain(self.tools.values_mut().map(|tool| &mut tool.original))
        {
            if let Err(error) = original.finish(control) {
                first.get_or_insert(error);
            }
        }
        if let Some(directory) = &mut self.private_directory
            && let Err(error) = directory.finish(control)
        {
            first.get_or_insert(error);
        }
        if let Some(work) = &mut self.work_owner
            && let Err(error) = work.finish(control)
        {
            first.get_or_insert(error);
        }
        first.map_or(Ok(()), Err)
    }
}
fn link_check(
    before: &Metadata,
    path: &std::path::Path,
    target: &std::path::Path,
    control: &mut Control,
) -> Result<()> {
    control.check()?;
    if !files::same(
        before,
        &fs::symlink_metadata(path).map_err(|_| Error::Changed)?,
    ) || fs::read_link(path).map_err(|_| Error::Io)? != target
        || fs::canonicalize(path).map_err(|_| Error::Io)? != target
    {
        return Err(Error::Changed);
    }
    control.check()
}
struct Call<'a> {
    role: &'a str,
    tool: &'a str,
    args: Vec<OsString>,
    cap: Duration,
    accepted: &'a [i32],
    input: Option<&'a File>,
    environment: Option<BTreeMap<OsString, OsString>>,
}
fn lookup(name: &str) -> Result<PathBuf> {
    let path = std::env::var_os("PATH").ok_or(Error::Input)?;
    std::env::split_paths(&path)
        .map(|part| part.join(name))
        .find(|candidate| {
            fs::metadata(candidate)
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
        .ok_or(Error::Refused)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn replacing_a_real_tool_alias_with_identical_text_is_still_changed() {
        let path = std::env::temp_dir().join(format!("maintain-link-{}", std::process::id()));
        fs::create_dir(&path).unwrap();
        let path = fs::canonicalize(path).unwrap();
        let target = path.join("tool");
        fs::write(&target, b"fixed tool").unwrap();
        let alias = path.join("alias");
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        let before = fs::symlink_metadata(&alias).unwrap();
        let mut control = Control::new(Instant::now()).unwrap();
        let original = link_check(&before, &alias, &target, &mut control);
        fs::remove_file(&alias).unwrap();
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        let changed = link_check(&before, &alias, &target, &mut control);
        control.settle();
        let finish = control.finish();
        fs::remove_dir_all(path).unwrap();
        assert_eq!(original, Ok(()));
        assert_eq!(changed, Err(Error::Changed));
        assert!(finish.is_ok());
    }
}
