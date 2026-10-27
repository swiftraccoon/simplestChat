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
) -> Result<(Tree, String)> {
    let mut owner = Some(tree);
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
        let saved = names.clone();
        let mut tree = owner.take().unwrap();
        let admitted = worker::work(
            plan,
            executor,
            end,
            move |checkpoint| {
                let admitted = seals.and_then(|seals| {
                    admit(
                        &mut tree,
                        &saved,
                        &seals,
                        &download,
                        successful,
                        &mut || checkpoint.check(),
                    )
                });
                if !successful || admitted.is_err() {
                    let closed = tree.finish();
                    return admitted.and(closed).and(Err(failure(1)));
                }
                Ok(tree)
            },
            Tree::finish,
        )
        .await;
        // An actual child/status failure precedes admission, close or worker
        // deadline failures. Never return a live Tree when that body failed.
        let tree = ran.and(admitted)?;
        Ok((tree, names[0].clone()))
    }
    .await;
    if let Some(tree) = owner.take() {
        let closed = tree.finish();
        return result.and_then(|v| closed.map(|()| v));
    }
    result
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
