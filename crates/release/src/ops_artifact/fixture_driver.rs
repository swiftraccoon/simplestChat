//! Inert Cargo/rustc roles for producer supervision tests. This std-only source
//! is also compiled once as a native executable by the enclosing test fixture.

#![forbid(unsafe_code)]
#![deny(warnings)]

use std::{
    ffi::{OsStr, OsString},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

const TARGET: &str = "x86_64-unknown-linux-musl";
const DATA: &str = "kit/toolchain/fixture";

fn read(path: &Path, maximum: u64) -> io::Result<Vec<u8>> {
    let file = File::open(path)?;
    let mut bytes = Vec::new();
    file.take(maximum + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(io::Error::other("fixture input exceeds bound"));
    }
    Ok(bytes)
}

pub(crate) fn arguments(root: &Path, metadata: bool) -> Vec<OsString> {
    let prefix = if metadata {
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
    };
    let mut arguments = prefix.into_iter().map(OsString::from).collect::<Vec<_>>();
    arguments.extend([
        "--no-default-features".into(),
        "--features".into(),
        "simplestchat-ops/maintain-guest".into(),
        "--manifest-path".into(),
        root.join("source/Cargo.toml").into(),
    ]);
    if !metadata {
        arguments.extend(["--target-dir".into(), root.join("target").into()]);
    }
    arguments
}

/// Execute only the fixed inert role; candidate bytes are written as mode0600
/// data and are never executed. The parent owns serialization and fault inputs.
pub(crate) fn execute(
    root: &Path,
    role: &OsStr,
    args: &[OsString],
    stdout: &mut impl Write,
    stderr: &mut impl Write,
) -> io::Result<u8> {
    if role == "rustc" {
        if args == ["-vV"] {
            stdout.write_all(
                concat!(
                    "rustc 1.99.0 (fixture 2026-10-22)\nrelease: 1.99.0\n",
                    "host: x86_64-unknown-linux-gnu\n"
                )
                .as_bytes(),
            )?;
        } else if args == ["--print", "sysroot"] {
            writeln!(stdout, "{}", root.join("kit/toolchain").display())?;
        } else {
            return Ok(96);
        }
    } else if role == "cargo" && args == ["--version", "--verbose"] {
        stdout.write_all(
            concat!(
                "cargo 1.99.0 (fixture 2026-10-22)\nrelease: 1.99.0\n",
                "host: x86_64-unknown-linux-gnu\n"
            )
            .as_bytes(),
        )?;
    } else if role == "cargo" && args == arguments(root, true) {
        let bytes = read(&root.join(DATA).join("metadata.json"), 16 * 1024 * 1024)?;
        stdout.write_all(&bytes)?;
        stdout.write_all(b"\n")?;
    } else if role == "cargo" && args == arguments(root, false) {
        let status = read(&root.join(DATA).join("build-status"), 4)?;
        let status = match status.as_slice() {
            b"0\n" => 0,
            b"100\n" => 100,
            _ => return Err(io::Error::other("unknown fixture build status")),
        };
        let bytes = read(&root.join(DATA).join("candidate.elf"), 16 * 1024 * 1024)?;
        let directory = root.join("target").join(TARGET).join("release");
        fs::create_dir_all(&directory)?;
        let mut candidate = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join("simplestchat-ops"))?;
        candidate.write_all(&bytes)?;
        candidate.sync_all()?;
        drop(candidate);
        stdout.write_all(b"PRIVATE-STDOUT\n")?;
        stderr.write_all(b"PRIVATE-STDERR\n")?;
        return Ok(status);
    } else {
        return Ok(97);
    }
    Ok(0)
}

#[cfg(not(test))]
fn main() -> std::process::ExitCode {
    let result = (|| {
        let mut arguments = std::env::args_os();
        let executable = arguments
            .next()
            .ok_or_else(|| io::Error::other("missing role"))?;
        let role = Path::new(&executable)
            .file_name()
            .ok_or_else(|| io::Error::other("invalid role"))?;
        execute(
            &std::env::current_dir()?,
            role,
            &arguments.collect::<Vec<_>>(),
            &mut io::stdout().lock(),
            &mut io::stderr().lock(),
        )
    })();
    std::process::ExitCode::from(result.unwrap_or(98))
}
