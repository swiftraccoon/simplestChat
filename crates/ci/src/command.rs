//! Closed command vocabulary for bounded CI subprocesses. No shell execution.

use simplestchat_toolkit::tooling::{Environment, Result};
use std::ffi::OsString;
use std::path::Path;
use std::time::Duration;

/// The closed set of programs a [`Command`] may run. Programs that need a
/// validated executable run only through their role's proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Program {
    Git,
    Docker,
    Curl,
    Sudo,
    Python,
    CargoAudit,
    Node,
    PgConfig,
    DtlsPython,
    DtlsWorker,
    OpsCargo,
    OpsRustc,
    OpsGit,
    OpsGh,
    OpsMaterialCurl,
    OpsDpkgDeb,
    OpsHostCc,
    OpsKitShell,
    OpsKitPerl,
    OpsKitMake,
    OpsRuntimeGit,
    OpsRuntimePython,
    OpsVpsGit,
    OpsVpsSsh,
    OpsVpsAnsible,
    FrontendGit,
    FrontendGh,
    FrontendInventory,
    FrontendAnsible,
    FrontendCurl,
    ComparisonGit,
    ComparisonCargo,
    ComparisonBenchmark,
    Zstd,
    MdnsInterfaces,
    MdnsPlist,
    MdnsLaunch,
    MdnsLogs,
    MdnsCompiler,
    MdnsProbe,
    BenchmarkCurl,
}
impl Program {
    /// The program's file name, or its fixed absolute path.
    pub fn name(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Docker => "docker",
            Self::Curl => "curl",
            Self::Sudo => "sudo",
            Self::Python => "python3",
            Self::CargoAudit => "cargo-audit",
            Self::Node => "node",
            Self::PgConfig => "pg_config",
            Self::DtlsPython => "python3",
            Self::DtlsWorker => "mediasoup-worker-test",
            Self::OpsCargo => "cargo",
            Self::OpsRustc => "rustc",
            Self::OpsGit => "git",
            Self::OpsGh => "gh",
            Self::OpsMaterialCurl => "curl",
            Self::OpsDpkgDeb => "dpkg-deb",
            Self::OpsHostCc => "cc",
            Self::OpsKitShell => "dash",
            Self::OpsKitPerl => "perl",
            Self::OpsKitMake => "make",
            Self::OpsRuntimeGit => "git",
            Self::OpsRuntimePython => "python3",
            Self::OpsVpsGit => "git",
            Self::OpsVpsSsh => "ssh",
            Self::OpsVpsAnsible => "ansible-playbook",
            Self::FrontendGit => "git",
            Self::FrontendGh => "gh",
            Self::FrontendInventory => "ansible-inventory",
            Self::FrontendAnsible => "ansible-playbook",
            Self::FrontendCurl => "curl",
            Self::ComparisonGit => "git",
            Self::ComparisonCargo => "cargo",
            Self::ComparisonBenchmark => "load_test",
            Self::Zstd => "zstd",
            Self::MdnsInterfaces => "/sbin/ifconfig",
            Self::MdnsPlist => "/usr/bin/plutil",
            Self::MdnsLaunch => "/bin/launchctl",
            Self::MdnsLogs => "/usr/bin/log",
            Self::MdnsCompiler => "/usr/bin/cc",
            Self::MdnsProbe => "owned-mdns-probe",
            Self::BenchmarkCurl => "nsenter",
        }
    }
}

/// The closed set of release proof roles. A role names the programs its
/// proofs may execute and selects the runner's rules for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseRole {
    Producer,
    SourceGit,
    Retained,
    Acquire,
    Hosted,
    Runtime,
    Vps,
    Frontend,
}
impl ReleaseRole {
    pub(crate) const ALL: [Self; 8] = [
        Self::Producer,
        Self::SourceGit,
        Self::Retained,
        Self::Acquire,
        Self::Hosted,
        Self::Runtime,
        Self::Vps,
        Self::Frontend,
    ];
    /// Whether this role's proofs may execute `program`. Retained and Acquire
    /// share `gh`; every other release program belongs to exactly one role.
    pub(crate) fn admits(self, program: Program) -> bool {
        match self {
            Self::Producer => matches!(program, Program::OpsCargo | Program::OpsRustc),
            Self::SourceGit => program == Program::OpsGit,
            Self::Retained | Self::Acquire => program == Program::OpsGh,
            Self::Hosted => matches!(
                program,
                Program::OpsMaterialCurl
                    | Program::OpsDpkgDeb
                    | Program::OpsHostCc
                    | Program::OpsKitShell
                    | Program::OpsKitPerl
                    | Program::OpsKitMake
            ),
            Self::Runtime => matches!(program, Program::OpsRuntimeGit | Program::OpsRuntimePython),
            Self::Vps => matches!(
                program,
                Program::OpsVpsGit | Program::OpsVpsSsh | Program::OpsVpsAnsible
            ),
            Self::Frontend => matches!(
                program,
                Program::FrontendGit
                    | Program::FrontendGh
                    | Program::FrontendInventory
                    | Program::FrontendAnsible
                    | Program::FrontendCurl
            ),
        }
    }
    /// Only an embedded source Git or producer compiler command may carry an
    /// original stage dispatch bound, and only for a program its role admits.
    pub(crate) fn bounds_dispatch(self, program: Program) -> bool {
        matches!(self, Self::Producer | Self::SourceGit) && self.admits(program)
    }
}

/// A release proof carried in [`Command::release`]. Its role selects which
/// of these the runner calls, and where; see `process.rs`. `Any` lets the
/// release plane recover its own concrete proof from a command it did not
/// build; the runner never names one.
pub trait ReleaseProof: std::any::Any + std::fmt::Debug + Send + Sync {
    fn role(&self) -> ReleaseRole;
    /// Admit the exact command once, before its owned streams are taken.
    fn begin(&self, command: &Command) -> Result<()>;
    /// The working directory and environment to run in. `None` selects the
    /// runner's own, which the runner allows only for a source Git proof.
    fn context(
        &self,
        program: Program,
        arguments: &[OsString],
    ) -> Result<Option<(&Path, &Environment)>>;
    /// Recheck the retained executable; returns the path to execute.
    fn validate(&self, program: Program, arguments: &[OsString]) -> Result<&Path>;
    /// The settled paired captures, or `None` when they are unproven.
    fn observed_io(&self, _: Option<super::bounded_log::Captures>) {}
    /// The reaped status (a negative signal number for a signalled exit),
    /// whether the group settled, and whether the command body completed
    /// within its window.
    fn observed_exit(&self, _status: Option<i32>, _settled: bool, _completed: bool) {}
    /// The final check after a successful command and its settled streams.
    fn completed(&self) -> Result<()> {
        Ok(())
    }
}

/// Independently bounded stdout bytes and private stderr. Native takes the one
/// owner and attempts both finalizers after retiring the command's group.
/// Callers retain distinct-file namespace, stamp and readback authority.
#[derive(Debug)]
pub struct SeparateLogs {
    pub stdout: super::bounded_log::Log,
    pub stderr: super::bounded_log::Log,
}

/// One bounded subprocess request: its program and arguments, deadlines,
/// output and input owners, and at most one proof for its executable role.
/// The fields the release control plane reads or writes, its tests included,
/// are public; the rest stay crate-private, so other crates build commands with
/// [`Command::capture`].
#[derive(Debug, Clone)]
pub struct Command {
    pub program: Program,
    pub arguments: Vec<OsString>,
    pub timeout: Duration,
    pub grace: Duration,
    pub capture: Option<u64>,
    pub cleanup: bool,
    pub stdout: Option<std::sync::Arc<std::fs::File>>,
    /// Unique owner; both streams are bounded and closed after group retirement.
    pub combined_log: Option<std::sync::Arc<super::bounded_log::Log>>,
    /// Mutually exclusive with capture, stdout and combined_log.
    pub separate_logs: Option<std::sync::Arc<SeparateLogs>>,
    /// Unique regular read-only input, consumed from its current cursor. The
    /// caller binds size and source stamps; Native explicitly closes this File.
    /// Its child-stdio duplicate is owned and closed by the standard library,
    /// which exposes no checked close result for that transferred descriptor.
    pub stdin: Option<std::sync::Arc<std::fs::File>>,
    pub(crate) executable: Option<std::sync::Arc<super::selected_executable::Executable>>,
    pub node: Option<std::sync::Arc<simplestchat_toolkit::executable::Executable>>,
    pub(crate) dtls: Option<std::sync::Arc<super::native_dtls::files::Executable>>,
    pub(crate) comparison: Option<std::sync::Arc<super::performance_compare::files::Invocation>>,
    pub(crate) zstd: Option<std::sync::Arc<simplestchat_toolkit::executable::Executable>>,
    /// Matching manual-owner Git/Docker role. Legacy bare requests stay valid;
    /// that owner's capture boundary must attach its selected proof every time.
    pub(crate) load_image: Option<std::sync::Arc<super::load_generator_image::tools::Invocation>>,
    pub(crate) mdns: Option<std::sync::Arc<super::mdns::tools::Invocation>>,
    pub(crate) benchmark_output:
        Option<std::sync::Arc<super::container_benchmark::tools::CaptureReceipt>>,
    pub(crate) benchmark: Option<std::sync::Arc<super::container_benchmark::tools::Invocation>>,
    /// The one release proof, whatever its role.
    pub release: Option<std::sync::Arc<dyn ReleaseProof>>,
    /// Original latest dispatch time for an embedded source/build command.
    /// This can only narrow a selected Ops Git/compiler invocation.
    pub ops_stage_dispatch: Option<std::time::Instant>,
    pub(crate) release_checks:
        Option<std::sync::Arc<super::release_checks::invocation::Invocation>>,
    #[cfg(feature = "release-http-policy")]
    pub(crate) release_http: Option<std::sync::Arc<super::release_http_policy::tools::Invocation>>,
}
impl Command {
    /// A command that captures up to 4 MiB of stdout, with a 30 s deadline, a
    /// 5 s grace and no proof; callers adjust the public fields before running it.
    pub fn capture(program: Program, args: &[&str]) -> Self {
        Self {
            program,
            arguments: args.iter().map(OsString::from).collect(),
            timeout: Duration::from_secs(30),
            grace: Duration::from_secs(5),
            capture: Some(4 * 1024 * 1024),
            cleanup: false,
            stdout: None,
            combined_log: None,
            separate_logs: None,
            stdin: None,
            executable: None,
            node: None,
            dtls: None,
            comparison: None,
            zstd: None,
            load_image: None,
            mdns: None,
            release: None,
            ops_stage_dispatch: None,
            benchmark: None,
            benchmark_output: None,
            release_checks: None,
            #[cfg(feature = "release-http-policy")]
            release_http: None,
        }
    }
}
