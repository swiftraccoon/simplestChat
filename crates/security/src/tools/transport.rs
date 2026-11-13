//! One bounded download: a few attempts within one deadline. What it returns
//! is not yet authenticated; `archive::selected`, the only reader of those
//! bytes, holds them to the lock's digest first.

use super::{Fault, Limits, Result, lock::Asset, require};
use std::{
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};

/// The whole of one tool's download, retries and their delays included.
const DOWNLOAD: Duration = Duration::from_secs(180);
const ATTEMPTS: u32 = 3;
/// The first delay; the second is twice as long.
const RETRY_DELAY: Duration = Duration::from_secs(2);
/// GitHub's release downloads answer these during its incidents. A bounded,
/// delayed retry rides one out; every other status is final.
const RETRIED: [u16; 4] = [500, 502, 503, 504];
const CURL: &str = "/usr/bin/curl";
/// How long after its own time limit a transfer is stopped from outside.
const GRACE: Duration = Duration::from_secs(10);
/// The most that is read of the one line a transfer reports.
const REPORT_MAX: u64 = 16 * 1024;

/// What one attempt at an address came to.
#[derive(Debug, PartialEq, Eq)]
pub enum Answer {
    /// A successful response's bytes and the address that finally served them.
    Body { bytes: Vec<u8>, address: String },
    /// A response with another status.
    Status(u16),
    /// More bytes than the caller's bound.
    Oversized,
    /// No answer within the time the caller gave.
    Late,
    /// No response at all, or one the transport would not follow.
    Failed,
}

/// The network and the clock a download runs against.
pub trait Transport {
    /// Time on a clock that never goes back.
    fn now(&mut self) -> Duration;
    /// Wait before another attempt.
    fn pause(&mut self, time: Duration);
    /// One attempt at `address` for at most `limit` bytes within `within`.
    fn fetch(&mut self, address: &str, limit: u64, within: Duration) -> Answer;
}

/// The bytes at `asset`'s address, within the bounds of size and time.
pub(super) fn download(
    asset: &Asset,
    limits: &Limits,
    transport: &mut dyn Transport,
) -> Result<Vec<u8>> {
    let deadline = transport.now() + DOWNLOAD;
    let mut attempt = 0;
    let (bytes, address) = loop {
        attempt += 1;
        let within = deadline.saturating_sub(transport.now());
        match transport.fetch(&asset.url, limits.archive, within) {
            Answer::Body { bytes, address } => break (bytes, address),
            Answer::Status(status) if RETRIED.contains(&status) && attempt < ATTEMPTS => {}
            Answer::Status(_) => return Err(Fault("tool_download_status")),
            Answer::Oversized => return Err(Fault("tool_download_size")),
            Answer::Late => return Err(Fault("tool_download_timeout")),
            Answer::Failed => return Err(Fault("tool_download_transport")),
        }
        let delay = RETRY_DELAY * attempt;
        require(transport.now() + delay < deadline, "tool_download_timeout")?;
        transport.pause(delay);
    };
    let secure = address
        .get(..8)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https://"));
    require(secure, "insecure_tool_redirect")?;
    require(transport.now() < deadline, "tool_download_timeout")?;
    require(bytes.len() as u64 <= limits.archive, "tool_download_size")?;
    Ok(bytes)
}

/// The system's curl, given nothing from the caller's environment.
pub struct Curl {
    started: Instant,
    pub(super) program: PathBuf,
    pub(super) grace: Duration,
}

impl Default for Curl {
    fn default() -> Self {
        Self::running(CURL.into(), GRACE)
    }
}

impl Curl {
    pub(super) fn running(program: PathBuf, grace: Duration) -> Self {
        Self {
            started: Instant::now(),
            program,
            grace,
        }
    }
}

/// HTTPS only, redirects included, with every bound curl itself can keep; the
/// body goes to its output and one line about the response to its error output.
pub(super) fn arguments(address: &str, limit: u64, within: Duration) -> Vec<String> {
    let mut arguments: Vec<String> = [
        "--disable",
        "--silent",
        "--globoff",
        "--location",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--tlsv1.2",
        "--max-redirs",
        "5",
        "--connect-timeout",
        "30",
        "--speed-limit",
        "1",
        "--speed-time",
        "30",
        "--output",
        "-",
        "--write-out",
        "%{stderr}%{http_code} %{url_effective}",
    ]
    .map(str::to_owned)
    .into();
    arguments.extend([
        "--max-time".to_owned(),
        within.as_secs().max(1).to_string(),
        "--max-filesize".to_owned(),
        limit.to_string(),
        "--url".to_owned(),
        address.to_owned(),
    ]);
    arguments
}

/// What a finished transfer says: how curl ended, whether it was stopped from
/// outside, the body it wrote and its one line about the response.
pub(super) fn judge(
    status: Option<i32>,
    stopped: bool,
    bytes: Vec<u8>,
    limit: u64,
    report: &[u8],
) -> Answer {
    if bytes.len() as u64 > limit || status == Some(63) {
        return Answer::Oversized;
    }
    if stopped || status == Some(28) {
        return Answer::Late;
    }
    let told = std::str::from_utf8(report)
        .ok()
        .and_then(|line| line.split_once(' '))
        .and_then(|(code, address)| Some((code.parse::<u16>().ok()?, address)));
    match (status, told) {
        (Some(0), Some((200..=299, address))) => Answer::Body {
            bytes,
            address: address.to_owned(),
        },
        (Some(0), Some((code @ 100..=599, _))) => Answer::Status(code),
        _ => Answer::Failed,
    }
}

impl Transport for Curl {
    fn now(&mut self) -> Duration {
        self.started.elapsed()
    }

    fn pause(&mut self, time: Duration) {
        std::thread::sleep(time);
    }

    fn fetch(&mut self, address: &str, limit: u64, within: Duration) -> Answer {
        let spawned = Command::new(&self.program)
            .args(arguments(address, limit, within))
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();
        let Ok(mut child) = spawned else {
            return Answer::Failed;
        };
        // curl keeps its own time limit; this stops one that does not. The
        // child is reaped only after the watch has ended, so the identifier it
        // signals is still the child's.
        let (finished, watching) = mpsc::channel::<()>();
        let grace = self.grace;
        let identifier = rustix::process::Pid::from_child(&child);
        let watch = std::thread::spawn(move || {
            let expired =
                watching.recv_timeout(within + grace) == Err(mpsc::RecvTimeoutError::Timeout);
            if expired {
                let _ = rustix::process::kill_process(identifier, rustix::process::Signal::KILL);
            }
            expired
        });
        let mut bytes = Vec::new();
        let mut report = Vec::new();
        let mut complete = false;
        if let Some(output) = child.stdout.take() {
            complete = output
                .take(limit.saturating_add(1))
                .read_to_end(&mut bytes)
                .is_ok();
        }
        if bytes.len() as u64 > limit {
            let _ = child.kill();
        }
        if let Some(errors) = child.stderr.take() {
            complete &= errors.take(REPORT_MAX).read_to_end(&mut report).is_ok();
        }
        drop(finished);
        let stopped = watch.join().unwrap_or(true);
        let status = child.wait().ok().and_then(|status| status.code());
        if !complete {
            return Answer::Failed;
        }
        judge(status, stopped, bytes, limit, &report)
    }
}
