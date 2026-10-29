//! One inherited read-only witness, two fixed private bind roles and held-FD exec.
use super::context::{Context, stamp};
use super::*;
use std::{
    ffi::CString,
    fs::{self, File},
    io::Read,
    os::unix::{ffi::OsStrExt, fs::MetadataExt},
};

pub(super) struct Inherited {
    number: Option<i32>,
    before: Option<Value>,
}
impl Inherited {
    pub fn parse(args: &[OsString], enter: bool) -> Result<Self> {
        if !enter {
            return Ok(Self {
                number: None,
                before: None,
            });
        }
        require(args.len() == 3 && args[0] == "--enter")?;
        let raw = args[1]
            .to_str()
            .filter(|value| !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .ok_or(Error::Input)?;
        let number = raw
            .parse::<i32>()
            .ok()
            .filter(|value| *value > 2)
            .ok_or(Error::Input)?;
        let mut held = Self {
            number: Some(number),
            before: None,
        };
        let admitted = (|| {
            held.before = Some(stamp(
                &fs::metadata(format!("/proc/self/fd/{number}")).map_err(|_| Error::Io)?,
            )?);
            require(readonly(number)?)
        })();
        if let Err(first) = admitted {
            let _ = held.close();
            return Err(first);
        }
        Ok(held)
    }
    pub fn close(&mut self) -> Result<()> {
        self.number
            .take()
            .map(nix::unistd::close)
            .transpose()
            .map(|_| ())
            .map_err(|_| Error::Close)
    }
}
fn readonly(number: i32) -> Result<bool> {
    let file = File::from(
        rustix::fs::open(
            format!("/proc/self/fdinfo/{number}"),
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| Error::Io)?,
    );
    let mut raw = Vec::new();
    let read = (&file)
        .take(FRAME as u64 + 1)
        .read_to_end(&mut raw)
        .map_err(|_| Error::Io);
    let closed = nix::unistd::close(file).map_err(|_| Error::Close);
    read.and(closed)?;
    require(raw.len() <= FRAME)?;
    let text = std::str::from_utf8(&raw).map_err(|_| Error::Refused)?;
    let flags = text
        .lines()
        .filter_map(|line| line.strip_prefix("flags:"))
        .map(str::trim)
        .collect::<Vec<_>>();
    require(
        flags.len() == 1
            && !flags[0].is_empty()
            && flags[0].bytes().all(|byte| (b'0'..=b'7').contains(&byte)),
    )?;
    let flags = u64::from_str_radix(flags[0], 8).map_err(|_| Error::Refused)?;
    Ok(flags & u64::from(rustix::fs::OFlags::ACCMODE.bits()) == 0)
}
pub(super) fn private(context: &Context) -> Result<()> {
    context.fence()?.check()?;
    require(
        fs::metadata("/proc/self/ns/mnt")
            .map_err(|_| Error::Io)?
            .ino()
            != context.config()?.namespace,
    )?;
    let file = File::from(
        rustix::fs::open(
            "/proc/self/mountinfo",
            rustix::fs::OFlags::RDONLY | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| Error::Io)?,
    );
    let mut raw = Vec::new();
    let mut source = &file;
    let mut buffer = [0; CHUNK];
    let result = (|| {
        loop {
            context.fence()?.check()?;
            let size = source.read(&mut buffer).map_err(|_| Error::Io)?;
            context.fence()?.check()?;
            if size == 0 {
                break;
            }
            raw.extend_from_slice(&buffer[..size]);
            require(raw.len() <= 1024 * 1024)?;
        }
        require(!raw.is_empty())
    })();
    let closed = nix::unistd::close(file).map_err(|_| Error::Close);
    result.and(closed)?;
    private_mountinfo(&raw)?;
    context.fence()?.check()
}
pub(super) fn private_mountinfo(raw: &[u8]) -> Result<()> {
    require(!raw.is_empty() && raw.len() <= 1024 * 1024)?;
    let text = std::str::from_utf8(raw).map_err(|_| Error::Refused)?;
    for line in text.lines() {
        let (before, _) = line.split_once(" - ").ok_or(Error::Refused)?;
        let fields = before.split_whitespace().collect::<Vec<_>>();
        require(
            fields.len() >= 6
                && fields[6..].iter().all(|field| {
                    !["shared:", "master:", "propagate_from:"]
                        .iter()
                        .any(|prefix| field.starts_with(prefix))
                }),
        )?;
    }
    Ok(())
}
fn bind(context: &Context, source: &str, target: &str) -> Result<()> {
    require((source == DOCKER && target == ALIAS) || (source == INSTALLED && target == DOCKER))?;
    private(context)?;
    context.check()?;
    let result = rustix::mount::mount_bind(source, target).map_err(|_| Error::Io);
    let fence = context.fence()?.check();
    result.and(fence)
}
fn execute(context: &mut Context, original: usize, args: &[OsString], native: bool) -> Result<()> {
    context.check()?;
    let arguments = args
        .iter()
        .map(|value| CString::new(value.as_os_str().as_bytes()).map_err(|_| Error::Input))
        .collect::<Result<Vec<_>>>()?;
    let mut environment = vec![
        CString::new("PATH=/usr/sbin:/usr/bin:/sbin:/bin").map_err(|_| Error::Input)?,
        CString::new("LC_ALL=C").map_err(|_| Error::Input)?,
    ];
    if native {
        environment.push(CString::new(format!("HOME={ROOT}")).map_err(|_| Error::Input)?);
    }
    let file = context.take(original)?;
    let closed = context.close();
    let finished = context.finish();
    if let Err(first) = closed.and(finished) {
        let _ = nix::unistd::close(file);
        return Err(first);
    }
    // nix::unistd::fexecve is a safe SDK adapter. The original stays CLOEXEC;
    // successful kernel close/exec has no user-space close receipt.
    let executed = nix::unistd::fexecve(&file, &arguments, &environment);
    let error = match executed {
        Ok(never) => match never {},
        Err(_) => Error::Io,
    };
    let _ = nix::unistd::close(file);
    Err(error)
}
pub(super) fn docker(context: &mut Context, args: &[OsString]) -> Result<()> {
    let original = context.real()?;
    let mut arguments = vec![OsString::from(DOCKER)];
    arguments.extend_from_slice(args);
    execute(context, original, &arguments, false)
}
pub(super) fn enter(
    context: &mut Context,
    inherited: &mut Inherited,
    args: &[OsString],
) -> Result<()> {
    require(args.len() == 3 && args[0] == "--enter")?;
    let revision = args[2]
        .to_str()
        .filter(|value| hex(value, 40))
        .ok_or(Error::Input)?;
    let descriptor = inherited.number.ok_or(Error::Refused)?;
    let expected = &context.config()?.native_stamp;
    require(
        inherited.before.as_ref() == Some(expected)
            && readonly(descriptor)?
            && stamp(&fs::metadata(format!("/proc/self/fd/{descriptor}")).map_err(|_| Error::Io)?)?
                == *expected,
    )?;
    let native = context.open(INSTALLED, 256 * 1024 * 1024, 0o755)?;
    require(
        stamp(&context.original(native)?.before)? == context.config()?.native_stamp
            && context.original(native)?.hash(context.fence()?)? == context.config()?.native_sha,
    )?;
    inherited.close()?;
    let docker = context.open(DOCKER, 320 * 1024 * 1024, 0o755)?;
    require(
        stamp(&context.original(docker)?.before)? == context.config()?.docker_stamp
            && context.original(docker)?.hash(context.fence()?)? == context.config()?.docker_sha,
    )?;
    let placeholder = context.open(ALIAS, 0, 0o600)?;
    context.check()?;
    let mounted = bind(context, DOCKER, ALIAS);
    let closed = context.close_original(placeholder);
    mounted.and(closed)?;
    let alias = context.real()?;
    require(stamp(&context.original(alias)?.before)? == stamp(&context.original(docker)?.before)?)?;
    context.close_original(docker)?;
    context.check()?;
    bind(context, INSTALLED, DOCKER)?;
    require(
        stamp(&fs::metadata(DOCKER).map_err(|_| Error::Io)?)? == context.config()?.native_stamp,
    )?;
    context.check()?;
    let arguments = [
        "simplestchat-ops",
        "release",
        "deploy",
        "--revision",
        revision,
        "--quiet-seconds",
        "0",
        "--timeout-seconds",
        "1800",
        "--finalization-seconds",
        "60",
    ]
    .map(OsString::from);
    execute(context, native, &arguments, true)
}
