//! Preserve the hosted native-patch gate without shell pipelines or `strings`.

use super::{io, require};
use anyhow::Result;
use regex::Regex;
use rustix::process::{Pid, Signal, WaitId, WaitIdOptions};
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

const PREFIX: &str = "vendor/mediasoup-sys-0.19.0/subprojects";
const WRAPS: &[(&str, &str)] = &[
    (
        "abseil-cpp.wrap",
        "source_filename = abseil-cpp-20260817.0.tar.gz",
    ),
    (
        "abseil-cpp.wrap",
        "source_hash = f7e05179df39c45434cad433f5783840bb3788ef322976f9138bc6b72b3a107d",
    ),
    ("abseil-cpp.wrap", "patch_directory = abseil-cpp"),
    (
        "flatbuffers.wrap",
        "source_hash = ccbce58684691de1e7d51f5e87786266b37d06ab66e9dfe2d0ec106fe50aace0",
    ),
    (
        "libuv.wrap",
        "source_hash = cb0d6dd2128d5a95bd242c6cc982a24fe608fa93da57b6b4ec763b0018c53e64",
    ),
    (
        "unordered-dense.wrap",
        "source_hash = 06c262f9d7e1ff94d92e0359f89cc5d8632f32dfffb5e176a10516c509f5e5f2",
    ),
    (
        "catch2.wrap",
        "source_hash = 0957cae5821b17ce07f0833aaa52b5137643a8382203221f363a8303c109af34",
    ),
];
const FILES: &[(&str, &str)] = &[
    (
        "abseil-cpp/meson.build",
        "9d09ed3cbdd418c92ad582d590bce139873f7aed4468ce6cae31e4fd412b52ae",
    ),
    (
        "abseil-cpp/LICENSE.build",
        "7939f4c45423cec4a18236ad0a88570e33508dd7462e07b1038001f90ece65fb",
    ),
    (
        "flatbuffers/meson.build",
        "f8a08600662c6e46444257137838143c1a7f31156a7640948ba5583784286705",
    ),
    (
        "libuv/meson.build",
        "4ea34585b52222502e7cb9ffd963d9a0ff3c06a7c3251d6d09461490bd069056",
    ),
    (
        "libuv/meson_options.txt",
        "dc02dc5b7d5bd069782529f59012e7ac25b53325b7783609c77a3b36ca819a7c",
    ),
    (
        "libuv/link_file_in_build_dir.py",
        "cba566c9f026b7c23c2460c4262ddd420e4da63986a892653d4c15a0f9e6943d",
    ),
    (
        "libuv/LICENSE.build",
        "7939f4c45423cec4a18236ad0a88570e33508dd7462e07b1038001f90ece65fb",
    ),
    (
        "ankerl-unordered-dense/meson.build",
        "67fc3f19a4f3352e2383293cd68e36c899b7b59445d3bd7bf54f3523ac9a5166",
    ),
];
const OPENSSL: &[u8] = b"OpenSSL 4.1.0 23 Oct 2026";
const OLD_OPENSSL: &[u8] = b"OpenSSL 3.0.8";

fn contains(bytes: &[u8], needle: &[u8]) -> bool {
    bytes.windows(needle.len()).any(|window| window == needle)
}

/// Scan across chunk boundaries, with the same ceiling as backend receipts.
fn binary(path: &Path) -> Result<()> {
    const LIMIT: u64 = 1024 * 1024 * 1024;
    let file = io::regular(path, LIMIT, true)?;
    let mut file = file.take(LIMIT + 1);
    let mut bytes = vec![0; 65536 + OPENSSL.len()];
    let mut tail = 0;
    let mut read = 0_u64;
    let mut found = false;
    let mut forbidden = false;
    loop {
        let count = file.read(&mut bytes[tail..])?;
        if count == 0 {
            break;
        }
        read += count as u64;
        require(read <= LIMIT, "ci_native_binary_size")?;
        let end = tail + count;
        found |= contains(&bytes[..end], OPENSSL);
        forbidden |= contains(&bytes[..end], OLD_OPENSSL);
        tail = end.min(OPENSSL.len() - 1);
        bytes.copy_within(end - tail..end, 0);
    }
    require(found, "ci_native_openssl_version")?;
    require(!forbidden, "ci_native_old_openssl")
}

fn verify(root: &Path, mut capture: impl FnMut(&str, &[&str]) -> Result<Vec<u8>>) -> Result<()> {
    let lru = capture("cargo", &["tree", "--locked", "-i", "lru"])?;
    let lru = std::str::from_utf8(&lru)?;
    let pattern = Regex::new(r"(?m)^lru v0\.18\.5(?:\s|$)")?;
    require(pattern.is_match(lru), "ci_native_lru")?;
    let mediasoup = capture("cargo", &["tree", "--locked", "-i", "mediasoup-sys"])?;
    let mediasoup = std::str::from_utf8(&mediasoup)?;
    let pattern = Regex::new(r"mediasoup-sys v0\.19\.0 \(.*/vendor/mediasoup-sys-0\.19\.0\)$")?;
    require(
        mediasoup.lines().any(|line| pattern.is_match(line)),
        "ci_native_mediasoup",
    )?;
    for (name, pin) in WRAPS {
        let bytes = io::bounded(&root.join(PREFIX).join(name), 1024 * 1024)?;
        require(contains(&bytes, pin.as_bytes()), "ci_native_source_pin")?;
    }
    let abseil = io::bounded(&root.join(PREFIX).join("abseil-cpp.wrap"), 1024 * 1024)?;
    require(
        !contains(&abseil, b"source_filename = abseil-cpp-20240722.0.tar.gz"),
        "ci_native_old_abseil",
    )?;
    require(
        !contains(&abseil, b"source_fallback_url"),
        "ci_native_abseil_fallback",
    )?;
    for (name, digest) in FILES {
        let bytes = io::bounded(
            &root.join(PREFIX).join("packagefiles").join(name),
            4 * 1024 * 1024,
        )?;
        require(
            io::hex(&Sha256::digest(bytes)) == *digest,
            "ci_native_overlay_hash",
        )?;
    }
    binary(&root.join("target/debug/simplestChat"))?;
    let linked = capture("ldd", &["target/debug/simplestChat"])?;
    let pattern = Regex::new(r"lib(ssl|crypto)\.so")?;
    require(
        !pattern.is_match(std::str::from_utf8(&linked)?),
        "ci_native_dynamic_openssl",
    )
}

/// Only this fixed call graph reaches child commands; no command text is parsed.
fn capture(root: &Path, program: &str, arguments: &[&str]) -> Result<Vec<u8>> {
    capture_with_deadline(root, program, arguments, Duration::from_secs(60))
}

fn capture_with_deadline(
    root: &Path,
    program: &str,
    arguments: &[&str],
    deadline: Duration,
) -> Result<Vec<u8>> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let mut child = tokio::process::Command::new(program)
            .args(arguments)
            .current_dir(root)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let raw = child
            .id()
            .and_then(|pid| i32::try_from(pid).ok())
            .filter(|pid| *pid > 1)
            .ok_or_else(|| anyhow::anyhow!("ci_native_tool_identity"))?;
        let pid = Pid::from_raw(raw).expect("positive owned child pid");
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let work = async {
            let (status, output, _) = tokio::try_join!(
                exited(pid),
                read_limited(stdout, 4 * 1024 * 1024),
                read_limited(stderr, 65536),
            )?;
            require(status.exit_status() == Some(0), "ci_native_tool_failed")?;
            Ok(output)
        };
        let result = tokio::time::timeout(deadline, work).await;
        if !matches!(&result, Ok(Ok(_))) {
            // NOWAIT retains the leader until pipe draining/cleanup finishes,
            // so this process-group identity cannot be recycled underneath us.
            let _ = rustix::process::kill_process_group(pid, Signal::KILL);
            let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
        } else {
            child.wait().await?;
        }
        result.map_err(|_| anyhow::anyhow!("ci_native_tool_timeout"))?
    })
}

async fn read_limited(reader: impl tokio::io::AsyncRead + Unpin, limit: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit + 1).read_to_end(&mut bytes).await?;
    require(bytes.len() as u64 <= limit, "ci_native_tool_output")?;
    Ok(bytes)
}

async fn exited(pid: Pid) -> Result<rustix::process::WaitIdStatus> {
    loop {
        match rustix::process::waitid(
            WaitId::Pid(pid),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
        ) {
            Ok(Some(status)) => return Ok(status),
            Ok(None) | Err(rustix::io::Errno::INTR) => {}
            Err(_) => anyhow::bail!("ci_native_tool_wait"),
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub(super) fn run(root: &Path, arguments: &[String]) -> Result<std::process::ExitCode> {
    if !arguments.is_empty() {
        eprintln!("usage: cargo run --locked -q -p xtask -- ci native-security");
        return Ok(std::process::ExitCode::from(2));
    }
    match verify(root, |program, arguments| capture(root, program, arguments)) {
        Ok(()) => {
            println!("Verified native dependency pins, overlays and server OpenSSL linkage.");
            Ok(std::process::ExitCode::SUCCESS)
        }
        Err(error) => {
            eprintln!("Native security verification failed: {error}");
            Ok(std::process::ExitCode::FAILURE)
        }
    }
}

#[cfg(test)]
#[path = "native_security_tests.rs"]
mod tests;
