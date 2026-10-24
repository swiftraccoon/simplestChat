//! Fixed Linux/OpenSSL curl role, exclusive trust store and original archive sink.
use super::{
    super::{
        acquire::{
            grant::Grant,
            transport::{self, Completion, Invocation, Observation, Role},
        },
        control::{self, Cause, Owner, Result, Window},
        files::{self, Dir, Retained},
        verify::{
            process,
            tool::{Environment, Program, Tool},
        },
    },
    Access,
};
use std::{
    ffi::{OsStr, OsString},
    time::Duration,
};

pub(super) struct Downloader {
    tool: Option<Tool>,
    ca_parent: Option<Dir>,
    ca: Option<Retained>,
    ca_sha256: Option<String>,
    empty_parent: Option<Dir>,
    empty: Option<Dir>,
}
fn environment(source: &Environment) -> Environment {
    let mut result: Environment = source
        .iter()
        .filter(|(key, _)| {
            ["PATH", "HOME", "USER", "LOGNAME"]
                .iter()
                .any(|name| *key == OsStr::new(name))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    result.insert("LC_ALL".into(), "C".into());
    result
}
fn quote(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}
impl Downloader {
    pub fn new() -> Self {
        Self {
            tool: None,
            ca_parent: None,
            ca: None,
            ca_sha256: None,
            empty_parent: None,
            empty: None,
        }
    }
    fn check(&self, owner: &mut Owner) -> Result<()> {
        self.tool.as_ref().unwrap().bytes(owner)?;
        let ca = self.ca.as_ref().unwrap();
        if ca.hash(owner)? != *self.ca_sha256.as_ref().unwrap() {
            return control::failed(owner, Cause::Changed);
        }
        self.empty.as_ref().unwrap().empty(owner)
    }
    async fn prepare(&mut self, access: &Access, attempt: &Dir, owner: &mut Owner) -> Result<()> {
        self.tool = Some(Tool::resolve_program(
            &access.environment,
            Program::Curl,
            owner,
        )?);
        let settlement_end = owner
            .now()
            .checked_add(Duration::from_secs(30))
            .ok_or(Cause::Deadline)?
            .min(owner.body_end());
        let body_end = settlement_end
            .checked_sub(Duration::from_secs(10))
            .ok_or(Cause::Deadline)?;
        owner.check(Window::Body)?;
        let completed = process::run_small(
            owner,
            process::Invocation {
                program: self.tool.as_ref().unwrap().path().into(),
                arguments: vec!["--disable".into(), "--version".into()],
                environment: environment(&access.environment),
                body_end,
                settlement_end,
            },
        )
        .await;
        self.tool.as_ref().unwrap().bytes(owner)?;
        let bytes = completed.result?;
        let text = std::str::from_utf8(&bytes).map_err(|_| Cause::Command)?;
        let first = text.lines().next().ok_or(Cause::Command)?;
        let version = first
            .strip_prefix("curl ")
            .and_then(|s| s.split_ascii_whitespace().next())
            .ok_or(Cause::Command)?;
        let mut parts = version.split('.');
        let major = parts.next().and_then(|v| v.parse::<u64>().ok());
        let minor = parts.next().and_then(|v| v.parse::<u64>().ok());
        let protocols = text.lines().find_map(|s| s.strip_prefix("Protocols:"));
        if !matches!((major, minor), (Some(8), Some(4..)) | (Some(9..), Some(_)))
            || !first
                .split_ascii_whitespace()
                .any(|s| s.starts_with("OpenSSL/"))
            || !protocols.is_some_and(|s| s.split_ascii_whitespace().any(|v| v == "https"))
        {
            return control::failed(owner, Cause::Command);
        }
        self.ca_parent = Some(Dir::open(access.ca.parent().ok_or(Cause::Input)?, owner)?);
        self.ca = Some(Retained::open(
            self.ca_parent.as_ref().unwrap().clone(),
            access.ca.file_name().ok_or(Cause::Input)?,
            owner,
        )?);
        self.ca_sha256 = Some(self.ca.as_ref().unwrap().hash(owner)?);
        self.empty_parent = Some(attempt.clone());
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|_| Cause::Io)?;
        owner.check(Window::Body)?;
        self.empty = Some(attempt.create_child(
            OsStr::new(&format!(".curl-ca.{}", files::hex(&random))),
            owner,
        )?);
        #[cfg(test)]
        if access.fault == Some(super::Fault::CapathMember) {
            let mut extra = Retained::create(
                self.empty.as_ref().unwrap().clone(),
                "forbidden-ca.pem",
                owner,
            )?;
            let result = extra.write(b"inert additional CA", owner);
            extra.finish(owner);
            result?;
        }
        self.check(owner)
    }
    pub async fn run(
        &mut self,
        access: &Access,
        attempt: &Dir,
        grant: &Grant,
        expected: u64,
        digest: &str,
        owner: &mut Owner,
    ) -> Completion {
        if let Err(cause) = self.prepare(access, attempt, owner).await {
            owner.record(cause);
            return Completion {
                result: Err(cause),
                file: None,
                observation: Observation::default(),
            };
        }
        let setup =
            (|| {
                self.check(owner)?;
                let settlement_end = owner.body_end();
                let body_end = settlement_end
                    .checked_sub(Duration::from_secs(10))
                    .ok_or(Cause::Deadline)?;
                if owner.now() >= body_end {
                    return control::failed(owner, Cause::Deadline);
                }
                let mut arguments: Vec<OsString> = [
                    "--disable",
                    "--silent",
                    "--show-error",
                    "--http1.1",
                    "--include",
                    "--globoff",
                    "--path-as-is",
                    "--noproxy",
                    "*",
                    "--proxy",
                    "",
                    "--proto",
                    "=https",
                    "--max-redirs",
                    "0",
                    "--connect-timeout",
                    "15",
                    "--tlsv1.2",
                    "--no-ca-native",
                    "--cacert",
                ]
                .into_iter()
                .map(OsString::from)
                .collect();
                arguments.push(access.ca.clone().into_os_string());
                arguments.push("--capath".into());
                arguments.push(self.empty.as_ref().unwrap().path().into_os_string());
                arguments.extend([OsString::from("--config"), OsString::from("-")]);
                let config = format!(concat!(
                "url=\"https://{}/\"\nrequest-target=\"{}\"\n",
                "header=\"Accept: application/zip\"\nheader=\"Accept-Encoding: identity\"\n",
                "user-agent=\"simplestchat-release-receive\"\n",
            ),grant.host(),quote(grant.target())).into_bytes();
                Ok(Invocation {
                    program: self.tool.as_ref().unwrap().path().into(),
                    arguments,
                    environment: environment(&access.environment),
                    input: config,
                    role: Role::Receiver {
                        directory: attempt.clone(),
                        expected,
                        digest: digest.into(),
                    },
                    body_end,
                    settlement_end,
                    #[cfg(test)]
                    expire_after: None,
                })
            })();
        let mut completed = match setup {
            Ok(request) => transport::run(owner, request).await,
            Err(cause) => {
                owner.record(cause);
                Completion {
                    result: Err(cause),
                    file: None,
                    observation: Observation::default(),
                }
            }
        };
        if completed.result.is_ok()
            && let Err(cause) = self.check(owner)
        {
            completed.result = Err(cause);
        }
        completed
    }
    pub fn finish(&mut self, owner: &mut Owner) {
        if let Some(empty) = self.empty.take()
            && let Err(cause) = empty.retire_empty(self.empty_parent.as_ref().unwrap(), owner)
        {
            owner.record(cause);
        }
        // This is a borrowed Rc clone of State's original attempt owner.
        drop(self.empty_parent.take());
        if let Some(ca) = self.ca.take() {
            ca.finish(owner);
        }
        if let Some(parent) = self.ca_parent.take() {
            parent.finish(owner);
        }
        if let Some(tool) = self.tool.take() {
            tool.finish(owner);
        }
    }
}
