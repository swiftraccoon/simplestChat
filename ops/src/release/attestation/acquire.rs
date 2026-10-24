//! Complete fresh API selection and local download/cached verification.
//! Explicit selectors and emitted public digests remain passive data.

#[path = "acquire/grant.rs"]
pub(super) mod grant;
#[path = "acquire/headers.rs"]
mod headers;
#[path = "acquire/licenses.rs"]
mod licenses;
#[path = "acquire/options.rs"]
pub(super) mod options;
#[cfg(test)]
#[path = "acquire/oracle_tests.rs"]
mod oracle_tests;
#[path = "acquire/policy.rs"]
mod policy;
#[cfg(test)]
#[path = "acquire/tests.rs"]
pub(super) mod tests;
#[path = "acquire/transport.rs"]
mod transport;

use super::{
    control::{self, Cause, Owner, Result, Window},
    files::{self, Dir, Retained},
    verify,
};
use options::Options;
use std::{
    ffi::{OsStr, OsString},
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};
use verify::tool::{self, Environment, Program, Tool};

const GH_LIMIT: usize = 65536;
const SETTLE: Duration = Duration::from_secs(10);
pub(super) struct VerifiedRelease {
    directory: PathBuf,
    envelope: Vec<u8>,
}
impl VerifiedRelease {
    pub(super) fn envelope(&self) -> &[u8] {
        &self.envelope
    }
}
pub(super) struct State {
    gh: Option<Tool>,
    curl: Option<Tool>,
    parent: Option<Dir>,
    attempt: Option<Dir>,
    artifact: Option<Dir>,
    zip: Option<Retained>,
    members: [Option<Retained>; 10],
    environment: Environment,
    #[cfg(test)]
    free_bytes: Option<u64>,
    #[cfg(test)]
    expire_after: Option<u64>,
}
impl State {
    pub(super) fn new(options: &Options) -> Self {
        let source: Environment = {
            #[cfg(test)]
            if let Some(environment) = &options.environment {
                environment.clone()
            } else {
                std::env::vars_os().collect()
            }
            #[cfg(not(test))]
            {
                let _ = options;
                std::env::vars_os().collect()
            }
        };
        Self::with_source(options, source)
    }
    pub(super) fn with_source(options: &Options, source: Environment) -> Self {
        #[cfg(not(test))]
        let _ = options;
        Self {
            gh: None,
            curl: None,
            parent: None,
            attempt: None,
            artifact: None,
            zip: None,
            members: std::array::from_fn(|_| None),
            environment: tool::restricted(source),
            #[cfg(test)]
            free_bytes: options.free_bytes,
            #[cfg(test)]
            expire_after: options.expire_after,
        }
    }
    fn tools(&self, owner: &mut Owner) -> Result<()> {
        if let Some(gh) = &self.gh {
            gh.check(owner)?;
        }
        if let Some(curl) = &self.curl {
            curl.check(owner)?;
        }
        Ok(())
    }
    pub(super) fn original_outputs(&self, owner: &mut Owner) -> Result<()> {
        for directory in [&self.parent, &self.attempt, &self.artifact]
            .into_iter()
            .flatten()
        {
            directory.check(owner, Window::Body)?;
        }
        if let Some(zip) = &self.zip {
            zip.check(owner, Window::Body)?;
        }
        for file in self.members.iter().flatten() {
            file.check(owner, Window::Body)?;
        }
        self.tools(owner)
    }
    fn windows_before(
        owner: &mut Owner,
        cap: Duration,
        ceiling: Instant,
    ) -> Result<(Instant, Instant)> {
        owner.check(Window::Body)?;
        let body = owner
            .body_end()
            .min(ceiling)
            .checked_sub(SETTLE)
            .ok_or(Cause::Deadline)?
            .min(owner.now().checked_add(cap).ok_or(Cause::Deadline)?);
        if body <= owner.now() {
            return control::failed(owner, Cause::Deadline);
        }
        Ok((body, body.checked_add(SETTLE).ok_or(Cause::Deadline)?))
    }
    async fn gh(&self, arguments: Vec<OsString>, owner: &mut Owner) -> Result<Vec<u8>> {
        self.gh_before(arguments, owner, owner.body_end()).await
    }
    async fn gh_before(
        &self,
        arguments: Vec<OsString>,
        owner: &mut Owner,
        ceiling: Instant,
    ) -> Result<Vec<u8>> {
        self.tools(owner)?;
        let (body_end, settlement_end) =
            Self::windows_before(owner, Duration::from_secs(20), ceiling)?;
        #[cfg(test)]
        LAST_PHASE.set("command");
        let completed = verify::process::run_small(
            owner,
            verify::process::Invocation {
                program: self.gh.as_ref().unwrap().path().into(),
                arguments,
                environment: self.environment.clone(),
                body_end,
                settlement_end,
            },
        )
        .await;
        self.tools(owner)?;
        let bytes = completed.result?;
        if bytes.len() > GH_LIMIT {
            return control::failed(owner, Cause::Bounds);
        }
        owner.check(Window::Body)?;
        Ok(bytes)
    }
    async fn api(&self, endpoint: String, owner: &mut Owner) -> Result<crate::json::Json> {
        let bytes = self
            .gh(
                vec![
                    "api".into(),
                    "--hostname".into(),
                    "github.com".into(),
                    "-H".into(),
                    "Accept: application/vnd.github+json".into(),
                    "-H".into(),
                    "X-GitHub-Api-Version: 2022-11-28".into(),
                    endpoint.into(),
                ],
                owner,
            )
            .await?;
        crate::json::decode_limited(&bytes, GH_LIMIT).map_err(|_| Cause::Policy)
    }
    pub(super) async fn metadata(
        &mut self,
        options: &Options,
        owner: &mut Owner,
    ) -> Result<verify::options::Metadata> {
        #[cfg(test)]
        LAST_PHASE.set("metadata-tool");
        self.gh = Some(Tool::resolve(&self.environment, owner)?);
        let selected = &options.selected;
        #[cfg(test)]
        LAST_PHASE.set("artifact-api");
        let artifact = self
            .api(
                format!(
                    "repos/{}/actions/artifacts/{}",
                    selected.repository,
                    selected.artifact()
                ),
                owner,
            )
            .await?;
        #[cfg(test)]
        LAST_PHASE.set("artifact-policy");
        let artifact = policy::artifact(&artifact, selected, owner)?;
        #[cfg(test)]
        LAST_PHASE.set("run-api");
        let run = self
            .api(
                format!(
                    "repos/{}/actions/runs/{}",
                    selected.repository, artifact.run
                ),
                owner,
            )
            .await?;
        #[cfg(test)]
        LAST_PHASE.set("run-policy");
        policy::run(&run, &artifact, selected, owner)
    }
    async fn curl_version_before(&mut self, owner: &mut Owner, ceiling: Instant) -> Result<()> {
        if let Some(curl) = &self.curl {
            return curl.bytes(owner);
        }
        #[cfg(test)]
        LAST_PHASE.set("curl-tool");
        self.curl = Some(Tool::resolve_program(
            &self.environment,
            Program::Curl,
            owner,
        )?);
        self.tools(owner)?;
        let (body_end, settlement_end) =
            Self::windows_before(owner, Duration::from_secs(20), ceiling)?;
        let completed = verify::process::run_small(
            owner,
            verify::process::Invocation {
                program: self.curl.as_ref().unwrap().path().into(),
                arguments: vec!["--disable".into(), "--version".into()],
                environment: curl_environment(&self.environment),
                body_end,
                settlement_end,
            },
        )
        .await;
        self.tools(owner)?;
        let bytes = completed.result?;
        let text = std::str::from_utf8(&bytes).map_err(|_| Cause::Command)?;
        let version = text
            .lines()
            .next()
            .and_then(|line| line.strip_prefix("curl "))
            .and_then(|line| line.split_ascii_whitespace().next())
            .ok_or(Cause::Command)?;
        let mut parts = version.split('.');
        let major = parts.next().and_then(|value| value.parse::<u64>().ok());
        let minor = parts.next().and_then(|value| value.parse::<u64>().ok());
        let https = text
            .lines()
            .find_map(|line| line.strip_prefix("Protocols:"))
            .is_some_and(|line| line.split_ascii_whitespace().any(|part| part == "https"));
        if !matches!((major, minor), (Some(9..), Some(_)) | (Some(8), Some(4..))) || !https {
            return control::failed(owner, Cause::Command);
        }
        Ok(())
    }
    async fn curl(
        &mut self,
        input: Vec<u8>,
        role: transport::Role,
        cap: Duration,
        owner: &mut Owner,
    ) -> Result<Option<String>> {
        self.curl_before(input, role, cap, owner, owner.body_end())
            .await
    }
    async fn curl_before(
        &mut self,
        input: Vec<u8>,
        role: transport::Role,
        cap: Duration,
        owner: &mut Owner,
        ceiling: Instant,
    ) -> Result<Option<String>> {
        self.original_outputs(owner)?;
        let (body_end, settlement_end) = Self::windows_before(owner, cap, ceiling)?;
        let api = matches!(role, transport::Role::Api);
        let completion = transport::run(
            owner,
            transport::Invocation {
                program: self.curl.as_ref().unwrap().path().into(),
                arguments: curl_arguments(),
                environment: curl_environment(&self.environment),
                input,
                role,
                body_end,
                settlement_end,
                #[cfg(test)]
                expire_after: self.expire_after,
            },
        )
        .await;
        // The original output owner survives even negative transport settlement.
        // Take it before propagating any primary error; finish closes it later.
        if let Some(file) = completion.file {
            self.zip = Some(file);
        }
        let result = completion.result;
        let observed = &completion.observation;
        if result.is_ok()
            && (!observed.input_complete
                || !observed.input_closed
                || !observed.output_closed
                || !observed.error_closed
                || !observed.error_eof
                || !observed.group_retired
                || !observed.reaped
                || observed.exit_status.is_none()
                || observed.stderr_bytes > 65536
                || !options::digest(&observed.stderr_sha256, 64)
                || (!api
                    && (observed.exit_status != Some(0)
                        || !observed.output_eof
                        || !observed.natural_output_eof
                        || observed.planned_header_stop
                        || observed.body_bytes == 0))
                || (api && !observed.planned_header_stop && observed.exit_status != Some(0)))
        {
            owner.record(Cause::Close);
        }
        self.original_outputs(owner)?;
        result
    }
    fn parent(&mut self, path: &Path, owner: &mut Owner) -> Result<()> {
        if !files::canonical(path) {
            return control::failed(owner, Cause::Input);
        }
        self.parent = Some(match path.symlink_metadata() {
            Ok(_) => Dir::open(path, owner)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let parent = path.parent().ok_or(Cause::Input)?;
                let name = path.file_name().ok_or(Cause::Input)?;
                let ancestor = Dir::open(parent, owner)?;
                let selected = ancestor.create_child(name, owner);
                ancestor.finish(owner);
                selected?
            }
            Err(error) => return control::io(owner, Err(error)),
        });
        owner.check(Window::Body)?;
        Ok(())
    }
    fn attempt(&mut self, owner: &mut Owner) -> Result<PathBuf> {
        let mut random = [0; 16];
        owner.check(Window::Body)?;
        getrandom::fill(&mut random).map_err(|_| Cause::Io)?;
        owner.check(Window::Body)?;
        let name = format!("release-verification.{}", files::hex(&random));
        self.attempt = Some(
            self.parent
                .as_ref()
                .unwrap()
                .create_child(OsStr::new(&name), owner)?,
        );
        Ok(self.attempt.as_ref().unwrap().path().join("artifact"))
    }
    async fn perform(&mut self, options: &Options, owner: &mut Owner) -> Result<Vec<u8>> {
        let metadata = self.metadata(options, owner).await?;
        let verified = self.verify_selected(options, metadata, owner, None).await?;
        self.summary(options, verified, owner)
    }
    pub(super) async fn verify_selected(
        &mut self,
        options: &Options,
        metadata: verify::options::Metadata,
        owner: &mut Owner,
        destination: Option<&Dir>,
    ) -> Result<VerifiedRelease> {
        let directory = if let Some(directory) = &options.artifact {
            // This branch deliberately never resolves curl, requests a token or
            // asks GitHub for a download URL, including on cached success.
            directory.clone()
        } else {
            let directory = if let Some(destination) = destination {
                destination.check(owner, Window::Body)?;
                destination.path().join("verified")
            } else {
                #[cfg(test)]
                LAST_PHASE.set("parent");
                self.parent(&options.output_parent, owner)?;
                #[cfg(test)]
                LAST_PHASE.set("attempt");
                self.attempt(owner)?
            };
            // Keep token and header acquisition before the artifact-directory
            // creation, preserving the reference's failed-evidence prefix.
            let grant = self
                .new_grant(&options.selected, owner, owner.body_end())
                .await?;
            #[cfg(test)]
            LAST_PHASE.set("artifact-dir");
            self.artifact = Some(if let Some(destination) = destination {
                destination.create_child(OsStr::new("verified"), owner)?
            } else {
                self.attempt
                    .as_ref()
                    .unwrap()
                    .create_child(OsStr::new("artifact"), owner)?
            });
            #[cfg(test)]
            LAST_PHASE.set("disk");
            let free = self.artifact.as_ref().unwrap().free_bytes(owner)?;
            #[cfg(test)]
            let free = self.free_bytes.unwrap_or(free);
            let needed = metadata
                .zip_bytes()
                .checked_add(verify::zip::MAX_ZIP)
                .and_then(|value| value.checked_add(384 * 1024 * 1024))
                .ok_or(Cause::Bounds)?;
            if free < needed {
                return control::failed(owner, Cause::Policy);
            }
            let input = blob_config(&grant)?;
            let role = transport::Role::Blob {
                directory: self.artifact.as_ref().unwrap().clone(),
                expected: metadata.zip_bytes(),
                digest: metadata.zip_sha256().into(),
            };
            #[cfg(test)]
            LAST_PHASE.set("blob");
            self.curl(input, role, Duration::from_secs(240), owner)
                .await?;
            #[cfg(test)]
            LAST_PHASE.set("extract");
            verify::zip::extract(
                self.zip.as_ref().unwrap(),
                self.artifact.as_ref().unwrap(),
                &mut self.members,
                &metadata,
                owner,
            )?;
            directory
        };
        self.original_outputs(owner)?;
        // The fresh operation intentionally has two ZIP passes: extraction above
        // and the unchanged cached member comparison under this SAME owner.
        let envelope =
            verify::acquired(directory.clone(), metadata, self.environment.clone(), owner).await?;
        self.original_outputs(owner)?;
        self.gh.as_ref().unwrap().bytes(owner)?;
        if let Some(curl) = &self.curl {
            curl.bytes(owner)?;
        }
        Ok(VerifiedRelease {
            directory,
            envelope,
        })
    }
    fn summary(
        &self,
        options: &Options,
        verified: VerifiedRelease,
        owner: &mut Owner,
    ) -> Result<Vec<u8>> {
        let VerifiedRelease {
            directory,
            envelope,
        } = verified;
        #[cfg(test)]
        LAST_PHASE.set("result");
        let value = crate::json::decode_limited(&envelope, GH_LIMIT).map_err(|_| Cause::Policy)?;
        let digests = value
            .get("attestation")
            .and_then(|value| value.get("predicate"))
            .and_then(|value| value.get("fileDigests"))
            .ok_or(Cause::Policy)?;
        let image = digests
            .get("image.tar")
            .and_then(crate::json::Json::string)
            .ok_or(Cause::Policy)?;
        let manifest = digests
            .get("release.json")
            .and_then(crate::json::Json::string)
            .ok_or(Cause::Policy)?;
        self.gh.as_ref().unwrap().bytes(owner)?;
        if let Some(curl) = &self.curl {
            curl.bytes(owner)?;
        }
        #[cfg(test)]
        let directory = options.output_spelling.as_ref().unwrap_or(&directory);
        let directory_text = directory.to_str().ok_or(Cause::Input)?;
        let mut bytes = serde_json::to_vec(&serde_json::json!({
            "revision":options.selected.revision,"directory":directory_text,
            "image.tar":image,"release.json":manifest
        }))
        .map_err(|_| Cause::Policy)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
    pub(super) async fn new_grant(
        &mut self,
        selected: &options::Selected,
        owner: &mut Owner,
        ceiling: Instant,
    ) -> Result<grant::Grant> {
        #[cfg(test)]
        LAST_PHASE.set("curl-version");
        self.curl_version_before(owner, ceiling).await?;
        #[cfg(test)]
        LAST_PHASE.set("token");
        let bytes = self
            .gh_before(
                vec![
                    "auth".into(),
                    "token".into(),
                    "--hostname".into(),
                    "github.com".into(),
                ],
                owner,
                ceiling,
            )
            .await?;
        #[cfg(test)]
        LAST_PHASE.set("token-policy");
        let token = grant::token(&bytes, owner)?;
        let input = api_config(selected, &token)?;
        #[cfg(test)]
        LAST_PHASE.set("grant");
        let location = self
            .curl_before(
                input,
                transport::Role::Api,
                Duration::from_secs(20),
                owner,
                ceiling,
            )
            .await?
            .ok_or(Cause::Policy)?;
        #[cfg(test)]
        LAST_PHASE.set("grant-policy");
        grant::Grant::decode(&location, owner)
    }
    pub(super) fn finish(mut self, owner: &mut Owner) {
        for file in self.members.iter_mut().filter_map(Option::take) {
            file.finish(owner);
        }
        if let Some(file) = self.zip.take() {
            file.finish(owner);
        }
        for directory in [&mut self.artifact, &mut self.attempt, &mut self.parent]
            .into_iter()
            .filter_map(Option::take)
        {
            directory.finish(owner);
        }
        if let Some(curl) = self.curl.take() {
            curl.finish(owner);
        }
        if let Some(gh) = self.gh.take() {
            gh.finish(owner);
        }
        let _ = owner.check(Window::Final);
    }
}
async fn execute(options: &Options, owner: &mut Owner) -> Result<Vec<u8>> {
    let mut state = State::new(options);
    let result = state.perform(options, owner).await;
    if let Err(cause) = &result {
        owner.record(*cause);
    }
    state.finish(owner);
    owner.error().map_or(result, Err)
}
fn curl_environment(environment: &Environment) -> Environment {
    environment
        .iter()
        .filter(|(key, _)| {
            ["PATH", "HOME", "USER", "LOGNAME", "LC_ALL"]
                .iter()
                .any(|name| *key == OsStr::new(name))
        })
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}
fn curl_arguments() -> Vec<OsString> {
    [
        "--disable",
        "--silent",
        "--http1.1",
        "--include",
        "--path-as-is",
        "--globoff",
        "--noproxy",
        "*",
        "--proxy",
        "",
        "--proto",
        "=https",
        "--max-redirs",
        "0",
        "--retry",
        "0",
        "--config",
        "-",
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}
fn quoted(value: &str) -> Result<String> {
    if !value.bytes().all(|byte| (32..127).contains(&byte)) {
        return Err(Cause::Policy);
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}
fn config(
    origin: &str,
    target: &str,
    user_agent: &str,
    connect: u8,
    headers: &[String],
) -> Result<Vec<u8>> {
    let mut frame = format!(
        "url = {}\nrequest-target = {}\nuser-agent = {}\nconnect-timeout = {connect}\n",
        quoted(origin)?,
        quoted(target)?,
        quoted(user_agent)?
    );
    for header in headers {
        frame.push_str(&format!("header = {}\n", quoted(header)?));
    }
    if frame.len() > 32768 {
        return Err(Cause::Bounds);
    }
    Ok(frame.into_bytes())
}
fn api_config(selected: &options::Selected, token: &str) -> Result<Vec<u8>> {
    config(
        "https://api.github.com/",
        &format!(
            "/repos/{}/actions/artifacts/{}/zip",
            selected.repository,
            selected.artifact()
        ),
        "simplestchat-release-fetch",
        20,
        &[
            format!("Authorization: Bearer {token}"),
            "Accept: application/vnd.github+json".into(),
            "X-GitHub-Api-Version: 2022-11-28".into(),
            "Accept-Encoding: identity".into(),
        ],
    )
}
fn blob_config(grant: &grant::Grant) -> Result<Vec<u8>> {
    config(
        &format!("https://{}/", grant.host()),
        grant.target(),
        "",
        15,
        &[
            "Accept: application/zip".into(),
            "Accept-Encoding: identity".into(),
        ],
    )
}
#[cfg(test)]
thread_local! {
    static COMPLETION_OFFSET: std::cell::Cell<Option<Duration>> = const {
        std::cell::Cell::new(None)
    };
    static LAST_PHASE: std::cell::Cell<&'static str> = const {
        std::cell::Cell::new("entry")
    };
    static FRAME_CLOSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static COMPLETION_SAW_CLOSED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
fn lifecycle(
    started: Instant,
    whole: Duration,
    operation: impl FnOnce() -> (bool, Option<Owner>),
) -> ExitCode {
    let Some(end) = started.checked_add(whole) else {
        return ExitCode::from(2);
    };
    let previous = nix::sys::stat::umask(nix::sys::stat::Mode::from_bits_truncate(0o077));
    let (passed, owner) = operation();
    let _ = nix::sys::stat::umask(previous);
    let finished = owner.is_some_and(|owner| owner.finish().is_ok());
    let completed = Instant::now();
    #[cfg(test)]
    let completed = {
        COMPLETION_SAW_CLOSED.set(FRAME_CLOSED.get());
        COMPLETION_OFFSET
            .get()
            .and_then(|offset| end.checked_add(offset))
            .unwrap_or(completed)
    };
    if passed && finished && completed < end {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
/// Verify explicit GitHub identities and signed server bytes locally.
///
/// A dedicated synchronous invocation owns the required original whole and
/// finalization budgets. Cached candidates still receive two fresh API calls
/// and four cryptographic verifier calls, with no curl or token/download role.
/// Fresh candidates use curl 8.4+ for TLS and retain failed private evidence.
/// Exit0 and the complete four-key result are both required; any nonzero status
/// invalidates all partial or previously emitted bytes. Usage failures are2;
/// operational failures are1. This does not authenticate an installed binary or
/// authorize deployment. No production caller is switched by this API.
///
/// # Panics
///
/// A valid operation must run outside an entered Tokio runtime. Starting or
/// dropping its runtime inside another asynchronous runtime can panic. Help and
/// license notices return before runtime/environment/file effects.
///
/// ```
/// use simplestchat_ops::release::verify;
/// use std::{ffi::OsString, process::ExitCode};
/// assert_eq!(verify::run(&[OsString::from("--help")]), ExitCode::SUCCESS);
/// ```
pub fn run(arguments: &[OsString]) -> ExitCode {
    let started = Instant::now();
    if arguments.len() == 1 && arguments[0] == "--licenses" {
        return if licenses::stdout().is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let options = match Options::parse(arguments) {
        Ok(None) => {
            return if super::prepare::stdout(format!("{}\n", options::USAGE).as_bytes()).is_ok() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            };
        }
        Ok(Some(options)) => options,
        Err(_) => {
            let _ = writeln!(io::stderr().lock(), "{}", options::USAGE);
            return ExitCode::from(2);
        }
    };
    lifecycle(started, options.whole, || {
        let mut owner = match Owner::new(started, options.whole, options.reserve) {
            Ok(owner) => owner,
            Err(_) => return (false, None),
        };
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => {
                owner.record(Cause::Io);
                return (false, Some(owner));
            }
        };
        let passed = runtime.block_on(async {
            let result = execute(&options, &mut owner).await;
            let passed = result.is_ok();
            let bytes = result.unwrap_or_else(|_| {
                b"{\"passed\":false,\"failureClass\":\"release_attestation_failed\"}\n".to_vec()
            });
            if let Err(cause) = verify::stdout(&mut owner, &bytes) {
                owner.record(cause);
            }
            passed && owner.check(Window::Final).is_ok()
        });
        drop(runtime);
        (passed, Some(owner))
    })
}

/// Existing exact scoped dependency notices, shared by the SSH controller.
pub(super) fn dependency_notices() -> io::Result<()> {
    licenses::stdout()
}
