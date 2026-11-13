//! `tools install [--tools NAME...] [--directory DIR] [--platform NAME]` and
//! `tools path NAME [--directory DIR] [--platform NAME]`.

use super::{Curl, Fault, Limits, Result, Site, Transport, install, path, platform};
use std::{
    io::Write,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::ExitCode,
};

const USAGE: &str = concat!(
    "usage: simplestchat-security tools install [--tools NAME...] [--directory DIR]",
    " [--platform NAME]\n",
    "       simplestchat-security tools path NAME [--directory DIR] [--platform NAME]",
);

/// What a command line asks for.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Request {
    /// `Some` for `path`, with the tool it names.
    pub name: Option<String>,
    pub tools: Vec<String>,
    pub directory: Option<PathBuf>,
    pub platform: Option<String>,
}

/// The request, or nothing for a command line that is not one of the two.
pub(super) fn parse(arguments: &[String]) -> Option<Request> {
    let (action, mut rest) = arguments.split_first()?;
    let mut request = Request::default();
    let installing = match action.as_str() {
        "install" => true,
        "path" => false,
        _ => return None,
    };
    let mut seen = Vec::new();
    while let Some((argument, after)) = rest.split_first() {
        rest = after;
        if !argument.starts_with("--") {
            if installing || request.name.is_some() {
                return None;
            }
            request.name = Some(argument.clone());
            continue;
        }
        if seen.contains(&argument) {
            return None;
        }
        seen.push(argument);
        let values = rest
            .iter()
            .take_while(|value| !value.starts_with("--"))
            .count();
        match (argument.as_str(), values) {
            ("--tools", 1..) if installing => {
                request.tools = rest[..values].to_vec();
                rest = &rest[values..];
            }
            ("--directory", 1..) => {
                request.directory = Some(PathBuf::from(&rest[0]));
                rest = &rest[1..];
            }
            ("--platform", 1..) => {
                request.platform = Some(rest[0].clone());
                rest = &rest[1..];
            }
            _ => return None,
        }
    }
    (installing || request.name.is_some()).then_some(request)
}

fn host() -> Result<&'static str> {
    let name = rustix::system::uname();
    let text = |part: &std::ffi::CStr| part.to_str().map(str::to_owned).unwrap_or_default();
    platform(&text(name.sysname()), &text(name.machine()))
}

pub(super) fn execute(
    root: &Path,
    request: &Request,
    transport: &mut dyn Transport,
) -> Result<PathBuf> {
    let platform = match &request.platform {
        Some(platform) => platform.as_str(),
        None => host()?,
    };
    let site = Site {
        lock: &root.join("build/security-tools.lock.json"),
        cache: &root.join(".cache/security-tools"),
        platform,
        limits: Limits::REVIEWED,
    };
    let directory = request.directory.as_deref();
    match &request.name {
        Some(name) => path(&site, name, directory),
        None => install(&site, &request.tools, directory, transport),
    }
}

/// Run one `tools` command for the checkout at `root`: the installation
/// directory or the tool's path on standard output, or one line naming the
/// failure's code on standard error and status 1.
pub fn run(root: &Path, arguments: &[String]) -> ExitCode {
    let Some(request) = parse(arguments) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    match execute(root, &request, &mut Curl::default()) {
        Ok(path) => {
            let mut line = path.as_os_str().as_bytes().to_vec();
            line.push(b'\n');
            match std::io::stdout().write_all(&line) {
                Ok(()) => ExitCode::SUCCESS,
                Err(_) => ExitCode::from(1),
            }
        }
        Err(Fault(code)) => {
            eprintln!("Security tool setup failed: {code}");
            ExitCode::from(1)
        }
    }
}
