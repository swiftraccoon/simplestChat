//! Credential-free fixed-origin material requests, original paired capture
//! seals, and independently checked complete archive bytes.

use super::{
    failure,
    files::Tree,
    plan::{self, Plan},
    tools::{InvocationSpec, Role, Tools},
    worker,
};
use crate::{
    ci::{command::Command, ops_artifact::verify::tools::Captures, process::Executor},
    tooling::{Environment, Result},
};
use std::{
    ffi::OsString,
    time::{Duration, Instant},
};

pub(super) struct Download {
    pub url: String,
    pub sha256: String,
    pub bytes: Option<u64>,
    pub maximum: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Timeout {
    ResolutionReported,
    TlsReported,
    ConnectUnspecified,
    OperationReported,
    Unknown,
}
impl Timeout {
    pub fn name(self) -> &'static str {
        match self {
            Self::ResolutionReported => "reported-resolution-timeout",
            Self::TlsReported => "reported-tls-timeout",
            Self::ConnectUnspecified => "reported-connect-timeout-unspecified",
            Self::OperationReported => "reported-operation-timeout",
            Self::Unknown => "unknown",
        }
    }
}
pub(super) struct Attempt {
    pub result: Result<(Tree, String)>,
    pub timeout: Option<Timeout>,
}
struct Audited {
    tree: Option<Tree>,
    timeout: Option<Timeout>,
}
impl Audited {
    fn finish(self) -> Result<()> {
        self.tree.map_or(Ok(()), Tree::finish)
    }
}

fn close_failed(
    tree: Tree,
    observation: Result<Option<Timeout>>,
    checkpoint: &mut impl FnMut() -> Result<()>,
) -> Result<Audited> {
    let closed = tree.finish();
    let timeout = observation.and_then(|value| closed.map(|()| value))?;
    checkpoint()?;
    Ok(Audited {
        tree: None,
        timeout,
    })
}

fn timeout(stderr: &[u8], url: &str) -> Timeout {
    let unknown = Timeout::Unknown;
    if !stderr.is_ascii() {
        return unknown;
    }
    let lines = stderr.split(|b| *b == b'\n').collect::<Vec<_>>();
    let [line, marker, empty] = lines.as_slice() else {
        return unknown;
    };
    let Some(code) = marker.strip_prefix(b"OPS_HTTP_STATUS:200:") else {
        return unknown;
    };
    if !empty.is_empty() || code.len() != 3 || !code.iter().all(u8::is_ascii_digit) {
        return unknown;
    }
    let Ok(status) = std::str::from_utf8(code).unwrap_or("").parse::<u16>() else {
        return unknown;
    };
    if status != 0 && !(100..=599).contains(&status) {
        return unknown;
    }
    let Some(line) = line.strip_prefix(b"curl: (28) ") else {
        return unknown;
    };
    let Ok(line) = std::str::from_utf8(line) else {
        return unknown;
    };
    let number = |v: &str| {
        !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()) && v.parse::<u64>().is_ok()
    };
    let words = line.split(' ').collect::<Vec<_>>();
    match words.as_slice() {
        ["SSL", "connection", "timeout"] => Timeout::TlsReported,
        ["Resolving", "timed", "out", "after", ms, "milliseconds"] if number(ms) => {
            Timeout::ResolutionReported
        }
        ["Connection", "timed", "out", "after", ms, "milliseconds"] if number(ms) => {
            Timeout::ConnectUnspecified
        }
        [
            "Failed",
            "to",
            "connect",
            "to",
            host,
            "port",
            port,
            "after",
            ms,
            "ms:",
            "Timeout",
            "was",
            "reached",
        ] if number(ms)
            && url::Url::parse(url).is_ok_and(|url| {
                url.host_str() == Some(*host)
                    && url
                        .port_or_known_default()
                        .is_some_and(|n| n.to_string() == *port)
            }) =>
        {
            Timeout::ConnectUnspecified
        }
        [
            "Operation",
            "timed",
            "out",
            "after",
            ms,
            "milliseconds",
            "with",
            count,
            "bytes",
            "received",
        ] if number(ms) && number(count) => Timeout::OperationReported,
        [
            "Operation",
            "timed",
            "out",
            "after",
            ms,
            "milliseconds",
            "with",
            count,
            "out",
            "of",
            total,
            "bytes",
            "received",
        ] if number(ms) && number(count) && number(total) => Timeout::OperationReported,
        _ => unknown,
    }
}
fn environment(tree: &Tree) -> Environment {
    [("LC_ALL", "C"), ("LANG", "C"), ("TZ", "UTC")]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .chain([("HOME".into(), tree.run.path.clone().into_os_string())])
        .collect()
}
fn arguments(url: &str, body: Duration) -> Vec<OsString> {
    [
        "--disable",
        "--fail",
        "--silent",
        "--show-error",
        "--location",
        "--max-redirs",
        "3",
        "--proto",
        "=https",
        "--proto-redir",
        "=https",
        "--tlsv1.2",
        "--noproxy",
        "*",
        "--connect-timeout",
        "10",
        "--max-time",
    ]
    .into_iter()
    .map(OsString::from)
    .chain([format!("{}.{:03}", body.as_secs(), body.subsec_millis()).into()])
    .chain(
        [
            "--retry",
            "0",
            "--write-out",
            "%{stderr}OPS_HTTP_STATUS:200:%{http_code}\n",
            "--url",
            url,
        ]
        .into_iter()
        .map(OsString::from),
    )
    .collect()
}

/// Complete paired seals remain cleanup evidence after a failed child. Only a
/// successful child may additionally authorize HTTP status and archive bytes.
/// The caller owns the Tree and must explicitly finish it on every refusal.
pub(super) fn admit(
    tree: &mut Tree,
    names: &[String; 2],
    seals: &Captures,
    download: &Download,
    successful: bool,
    checkpoint: &mut impl FnMut() -> Result<()>,
) -> Result<()> {
    tree.captured(names, seals, checkpoint)?;
    if successful {
        if tree.read(&names[1], 65536, checkpoint)? != b"OPS_HTTP_STATUS:200:200\n" {
            return Err(failure(1));
        }
        let record = &tree.records[&names[0]];
        if record.sha256 != download.sha256
            || record.bytes == 0
            || record.bytes > download.maximum
            || download.bytes.is_some_and(|n| n != record.bytes)
        {
            return Err(failure(1));
        }
        tree.check()?;
    }
    Ok(())
}

pub(super) async fn run<E: Executor>(
    tree: Tree,
    index: usize,
    download: Download,
    tools: &Tools,
    executor: &mut E,
    plan: &Plan,
    end: Instant,
) -> Attempt {
    let mut owner = Some(tree);
    let mut observed_timeout = None;
    let result = async {
        let tree = owner.as_ref().unwrap();
        let (body, latest) = plan.body(end, plan::DOWNLOAD)?;
        let args = arguments(&download.url, body);
        let proof = tools.invocation(InvocationSpec {
            role: Role::Curl,
            args: args.clone(),
            directory: tree.run.path.clone(),
            environment: environment(tree),
            body,
            stdout_maximum: download.maximum,
            input: None,
            clock: plan.clock.clone(),
            latest,
        })?;
        let (logs, names, permits) = tree.capture(index, download.maximum).await?;
        let mut command = Command::capture(Role::Curl.program(), &[]);
        command.arguments = args;
        command.timeout = body;
        command.grace = Duration::from_secs(5);
        command.capture = None;
        command.separate_logs = Some(logs);
        command.hosted_ops = Some(proof.clone());
        let ran = executor.run(command).await;
        drop(permits);
        let seals = proof.captures();
        drop(proof);
        let successful = ran.is_ok();
        let child_timeout = executor.cleanup_passed()
            && ran
                .as_ref()
                .is_err_and(|error| error.is_child_exit() && error.code == 28);
        let saved = names.clone();
        let mut tree = owner.take().unwrap();
        let admitted = worker::work(
            plan,
            executor,
            end,
            move |checkpoint| {
                let mut observed = None;
                let admitted = seals.and_then(|seals| {
                    admit(
                        &mut tree,
                        &saved,
                        &seals,
                        &download,
                        successful,
                        &mut || checkpoint.check(),
                    )?;
                    if child_timeout {
                        observed = Some(timeout(
                            &tree.read(&saved[1], 65536, &mut || checkpoint.check())?,
                            &download.url,
                        ));
                    }
                    Ok(())
                });
                if !successful || admitted.is_err() {
                    return close_failed(tree, admitted.map(|()| observed), &mut || {
                        checkpoint.check()
                    });
                }
                Ok(Audited {
                    tree: Some(tree),
                    timeout: None,
                })
            },
            Audited::finish,
        )
        .await;
        observed_timeout = admitted.as_ref().ok().and_then(|value| value.timeout);
        // An actual child/status failure precedes admission, close or worker
        // deadline failures. Never return a live Tree when that body failed.
        let tree = ran.and(admitted)?.tree.ok_or_else(|| failure(1))?;
        Ok((tree, names[0].clone()))
    }
    .await;
    if let Some(tree) = owner.take() {
        let closed = tree.finish();
        return Attempt {
            result: result.and_then(|v| closed.map(|()| v)),
            timeout: None,
        };
    }
    Attempt {
        result,
        timeout: observed_timeout,
    }
}

pub(super) async fn decoded_deb<E: Executor>(
    tree: Tree,
    index: usize,
    input_name: &str,
    tools: &Tools,
    executor: &mut E,
    plan: &Plan,
    end: Instant,
) -> Result<(Tree, String)> {
    let mut owner = Some(tree);
    let result = async {
        let tree = owner.as_ref().unwrap();
        let (body, latest) = plan.body(end, Duration::from_secs(30))?;
        let args = vec!["--fsys-tarfile".into(), "-".into()];
        let input_stamp = tree
            .records
            .get(input_name)
            .ok_or_else(|| failure(1))?
            .stamp
            .clone();
        let proof = tools.invocation(InvocationSpec {
            role: Role::DpkgDeb,
            args: args.clone(),
            directory: tree.run.path.clone(),
            environment: environment(tree),
            body,
            stdout_maximum: super::files::MAX_FILE,
            input: Some(input_stamp),
            clock: plan.clock.clone(),
            latest,
        })?;
        let (logs, names, permits) = tree.capture(index, super::files::MAX_FILE).await?;
        let mut command = Command::capture(Role::DpkgDeb.program(), &[]);
        command.arguments = args;
        command.timeout = body;
        command.grace = Duration::from_secs(5);
        command.capture = None;
        command.separate_logs = Some(logs);
        command.hosted_ops = Some(proof.clone());
        let (input, input_permit) = match tree.input(input_name) {
            Ok(v) => v,
            Err(error) => {
                let _ = crate::ci::process::settle_unforwarded(command).await;
                return Err(error);
            }
        };
        command.stdin = Some(input);
        let ran = executor.run(command).await;
        drop(input_permit);
        drop(permits);
        ran?;
        let seals = proof.captures()?;
        let saved = names.clone();
        let mut tree = owner.take().unwrap();
        let tree = worker::work(
            plan,
            executor,
            end,
            move |checkpoint| {
                let admitted = tree
                    .captured(&saved, &seals, &mut || checkpoint.check())
                    .and_then(|()| {
                        if tree.records[&saved[1]].bytes == 0 {
                            tree.check()
                        } else {
                            Err(failure(1))
                        }
                    });
                if let Err(error) = admitted {
                    let _ = tree.finish();
                    return Err(error);
                }
                Ok(tree)
            },
            Tree::finish,
        )
        .await?;
        Ok((tree, names[0].clone()))
    }
    .await;
    if let Some(tree) = owner.take() {
        let closed = tree.finish();
        return result.and_then(|v| closed.map(|()| v));
    }
    result
}

#[cfg(test)]
mod timeout_tests {
    use super::super::super::namespace::Budget;
    use super::super::fixture::Fixture;
    use super::*;
    use crate::tooling::Failure;

    const URL: &str = "https://git.musl-libc.org/cgit/musl/snapshot/archive.tar.gz";
    const TLS: &[u8] = b"curl: (28) SSL connection timeout\nOPS_HTTP_STATUS:200:000\n";

    #[test]
    fn malformed_or_secret_neighbors_stay_unknown_after_complete_read() {
        for bytes in [
            b"curl: (28) SSL connection timeout PRIVATE-token\nOPS_HTTP_STATUS:200:000\n".as_slice(),
            b"curl: (28) SSL connection timeout\nPRIVATE https://secret.invalid\nOPS_HTTP_STATUS:200:000\n",
            b"curl: (28) SSL connection timeout\nOPS_HTTP_STATUS:200:000\nOPS_PRIVATE_TIMING:secret\n",
            b"curl: (28) SSL connection timeout\nOPS_HTTP_STATUS:200:999\n",
            b"curl: (28) SSL connection timeout\nOPS_HTTP_STATUS:200:000",
            b"curl: (28) SSL connection timeout\0\nOPS_HTTP_STATUS:200:000\n",
            b"curl: (28) Resolving timed out after PRIVATE milliseconds\nOPS_HTTP_STATUS:200:000\n",
            b"curl: (28) Failed to connect to secret.invalid port 443 after 10012 ms: Timeout was reached\nOPS_HTTP_STATUS:200:000\n",
        ] {
            assert_eq!(timeout(bytes, URL), Timeout::Unknown);
        }
        assert_eq!(timeout(TLS, URL), Timeout::TlsReported);
    }

    #[test]
    fn read_finish_and_final_fence_failures_never_export_an_observation() {
        for fault in ["read", "finish", "late"] {
            let fixture = Fixture::new();
            let budget = Budget::default();
            let mut tree = Tree::create(&fixture.root.join("download"), &budget).unwrap();
            tree.write("stderr", TLS, false).unwrap();
            if fault == "read" {
                std::fs::write(tree.run.path.join("stderr"), b"PRIVATE changed stderr").unwrap();
            }
            let observation = tree
                .read("stderr", 65536, &mut || Ok(()))
                .map(|bytes| Some(timeout(&bytes, URL)));
            if fault == "finish" {
                std::fs::write(tree.run.path.join("unregistered"), b"PRIVATE").unwrap();
            }
            let audited = close_failed(tree, observation, &mut || {
                if fault == "late" {
                    Err(failure(124))
                } else {
                    Ok(())
                }
            });
            let unavailable = audited.is_err();
            let primary = Err::<(), _>(Failure::child_exit(28));
            let error = primary.and(audited).err().unwrap();
            assert_eq!(budget.count(), 0);
            assert_eq!(budget.close_counts().1, 0);
            assert!(unavailable, "{fault}");
            assert_eq!(error.code, 28);
            assert!(error.is_child_exit());
        }
    }
}
