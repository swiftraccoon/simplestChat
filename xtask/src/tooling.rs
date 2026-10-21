//! Shared child-command and pinned native build environment support.
//!
//! Environment changes belong to each child, never the running process. Tool
//! failures retain the status a shell reports, including 128 plus a signal.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};

pub(crate) type Environment = BTreeMap<OsString, OsString>;
pub(crate) type Result<T> = std::result::Result<T, Failure>;

/// An actionable refusal or child failure, with its original shell status.
#[derive(Debug)]
pub(crate) struct Failure {
    pub code: u8,
    pub message: String,
}

impl Failure {
    pub(crate) fn new(code: u8, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for Failure {}

pub(crate) fn inherited_env() -> Environment {
    std::env::vars_os().collect()
}

pub(crate) fn shell_status(status: ExitStatus) -> u8 {
    u8::try_from(
        status
            .code()
            .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)),
    )
    .unwrap_or(1)
}

/// Construct a child in the checkout with the supplied environment. `PWD`
/// follows the actual working directory, as it did after the shell's `cd`.
pub(crate) fn command<I, S>(
    root: &Path,
    environment: &Environment,
    program: impl AsRef<OsStr>,
    arguments: I,
) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut child = Command::new(program);
    child
        .args(arguments)
        .current_dir(root)
        .env_clear()
        .envs(environment)
        .env("PWD", root);
    child
}

fn launch_failure(program: &OsStr, error: io::Error) -> Failure {
    let code = match error.kind() {
        io::ErrorKind::NotFound => 127,
        io::ErrorKind::PermissionDenied => 126,
        _ => 1,
    };
    Failure::new(code, format!("{}: {error}", program.to_string_lossy()))
}

pub(crate) fn run<I, S>(
    root: &Path,
    environment: &Environment,
    program: impl AsRef<OsStr>,
    arguments: I,
) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let program = program.as_ref();
    let status = command(root, environment, program, arguments)
        .status()
        .map_err(|error| launch_failure(program, error))?;
    if status.success() {
        Ok(())
    } else {
        Err(Failure::new(
            shell_status(status),
            format!("{} failed ({status})", program.to_string_lossy()),
        ))
    }
}

/// Capture only stdout; the child's diagnostics remain visible on stderr.
pub(crate) fn output<I, S>(
    root: &Path,
    environment: &Environment,
    program: impl AsRef<OsStr>,
    arguments: I,
) -> Result<Vec<u8>>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let program = program.as_ref();
    let output = command(root, environment, program, arguments)
        .stderr(Stdio::inherit())
        .output()
        .map_err(|error| launch_failure(program, error))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(Failure::new(
            shell_status(output.status),
            format!("{} failed ({})", program.to_string_lossy(), output.status),
        ))
    }
}

pub(crate) fn pinned_toolchain(root: &Path) -> Result<String> {
    let text = std::fs::read_to_string(root.join("rust-toolchain.toml")).map_err(|error| {
        Failure::new(2, format!("Cannot read the pinned Rust toolchain: {error}"))
    })?;
    let channel = text.lines().find_map(|line| {
        let (name, value) = line.trim().split_once('=')?;
        if name.trim() != "channel" {
            return None;
        }
        value.trim().strip_prefix('"')?.strip_suffix('"')
    });
    channel
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| Failure::new(2, "Cannot read the pinned Rust toolchain."))
}

/// Pin compiler paths and put their directory before incompatible system tools.
pub(crate) fn pin_compilers(
    root: &Path,
    environment: &mut Environment,
    channel: &str,
) -> Result<()> {
    for (name, executable) in [("RUSTC", "rustc"), ("RUSTDOC", "rustdoc")] {
        let path = output(
            root,
            environment,
            "rustup",
            ["which", "--toolchain", channel, executable],
        )?;
        let path = String::from_utf8(path).map_err(|_| {
            Failure::new(2, format!("rustup returned an invalid {executable} path"))
        })?;
        environment.insert(name.into(), path.trim_end_matches('\n').into());
    }
    let compiler = Path::new(&environment[OsStr::new("RUSTC")]);
    let directory = compiler
        .parent()
        .ok_or_else(|| Failure::new(2, "rustup returned an invalid rustc path"))?;
    let mut paths = vec![directory.to_path_buf()];
    if let Some(path) = environment.get(OsStr::new("PATH")) {
        paths.extend(std::env::split_paths(path));
    }
    let path = std::env::join_paths(paths)
        .map_err(|error| Failure::new(2, format!("Cannot prepare compiler PATH: {error}")))?;
    environment.insert("PATH".into(), path);
    Ok(())
}

/// Native build inputs selected for a child; applying them never installs them.
pub(crate) struct NativeInputs {
    root: PathBuf,
    pub openssl_dir: PathBuf,
    pub aws_lc_dir: PathBuf,
    pub pip_constraint: PathBuf,
}

/// Apply the pinned static-library settings. Local launch forces repository
/// prefixes; quality checks may honor explicitly supplied nonempty prefixes.
pub(crate) fn native_env(
    root: &Path,
    environment: &mut Environment,
    respect_prefixes: bool,
) -> NativeInputs {
    let prefix = |name, default: &str| {
        if respect_prefixes {
            environment
                .get(OsStr::new(name))
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
                .unwrap_or_else(|| root.join(default))
        } else {
            root.join(default)
        }
    };
    let openssl_dir = prefix("OPENSSL_DIR", "target/openssl-4.0.3");
    let aws_lc_dir = prefix("AWS_LC_SYS_SYSTEM_DIR", "target/aws-lc-5.11.0");
    let pip_constraint = root.join("build/pip-constraints.txt");
    let inputs = NativeInputs {
        root: root.to_path_buf(),
        openssl_dir,
        aws_lc_dir,
        pip_constraint,
    };
    for (name, value) in [
        ("OPENSSL_DIR", inputs.openssl_dir.as_os_str()),
        ("AWS_LC_SYS_SYSTEM_DIR", inputs.aws_lc_dir.as_os_str()),
        ("PIP_CONSTRAINT", inputs.pip_constraint.as_os_str()),
    ] {
        environment.insert(name.into(), value.to_owned());
    }
    environment.insert(
        "PKG_CONFIG_PATH".into(),
        inputs.openssl_dir.join("lib/pkgconfig").into_os_string(),
    );
    for (name, value) in [
        ("OPENSSL_STATIC", "1"),
        ("AWS_LC_SYS_STATIC", "1"),
        ("AWS_LC_SYS_SYSTEM_SKIP_VERSION_CHECK", "0"),
    ] {
        environment.insert(name.into(), value.into());
    }
    inputs
}

pub(crate) fn require_native_archives(inputs: &NativeInputs) -> Result<()> {
    let openssl = inputs.root.join(&inputs.openssl_dir);
    let aws_lc = inputs.root.join(&inputs.aws_lc_dir);
    if !openssl.join("lib/libssl.a").is_file() || !openssl.join("lib/libcrypto.a").is_file() {
        return Err(Failure::new(
            2,
            "Static OpenSSL is missing; follow docs/development.md#native-and-web-build.",
        ));
    }
    if !aws_lc.join("lib/libcrypto-awslc.a").is_file()
        || !aws_lc.join("share/rust/aws_lc_bindings.rs").is_file()
    {
        return Err(Failure::new(
            2,
            "Static AWS-LC or its matching Rust bindings are missing; follow \
             docs/development.md#native-and-web-build.",
        ));
    }
    Ok(())
}
