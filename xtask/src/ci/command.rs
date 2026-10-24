//! Closed command vocabulary for bounded CI subprocesses. No shell execution.

use std::ffi::OsString;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Program {
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
    OpsRuntimeGit,
    OpsRuntimePython,
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
    pub(super) fn name(self) -> &'static str {
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
            Self::OpsRuntimeGit => "git",
            Self::OpsRuntimePython => "python3",
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

/// Independently bounded stdout bytes and private stderr. Native takes the one
/// owner and attempts both finalizers after retiring the command's group.
/// Callers retain distinct-file namespace, stamp and readback authority.
#[derive(Debug)]
pub(super) struct SeparateLogs {
    pub stdout: super::bounded_log::Log,
    pub stderr: super::bounded_log::Log,
}

#[derive(Debug, Clone)]
pub(super) struct Command {
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
    pub executable: Option<std::sync::Arc<super::selected_executable::Executable>>,
    pub node: Option<std::sync::Arc<super::release_checks::files::Executable>>,
    pub dtls: Option<std::sync::Arc<super::native_dtls::files::Executable>>,
    pub ops: Option<std::sync::Arc<super::ops_artifact::recipe::Tool>>,
    pub source_git: Option<std::sync::Arc<super::ops_artifact::source::Git>>,
    pub comparison: Option<std::sync::Arc<super::performance_compare::files::Invocation>>,
    pub zstd: Option<std::sync::Arc<super::release_checks::files::Executable>>,
    /// Matching manual-owner Git/Docker role. Legacy bare requests stay valid;
    /// that owner's capture boundary must attach its selected proof every time.
    pub load_image: Option<std::sync::Arc<super::load_generator_image::tools::Invocation>>,
    pub mdns: Option<std::sync::Arc<super::mdns::tools::Invocation>>,
    pub benchmark_output: Option<std::sync::Arc<super::container_benchmark::tools::CaptureReceipt>>,
    pub benchmark: Option<std::sync::Arc<super::container_benchmark::tools::Invocation>>,
    pub retained_ops: Option<std::sync::Arc<super::ops_artifact::verify::tools::Invocation>>,
    pub ops_acquire: Option<std::sync::Arc<super::ops_artifact::acquire::tools::Invocation>>,
    pub hosted_ops: Option<std::sync::Arc<super::ops_artifact::hosted::tools::Invocation>>,
    pub ops_runtime: Option<std::sync::Arc<super::ops_artifact::runtime::tools::Invocation>>,
    /// Original latest dispatch time for an embedded source/build command.
    /// This can only narrow a selected Ops Git/compiler invocation.
    pub ops_stage_dispatch: Option<std::time::Instant>,
    pub release_checks: Option<std::sync::Arc<super::release_checks::invocation::Invocation>>,
    #[cfg(feature = "release-http-policy")]
    pub release_http: Option<std::sync::Arc<super::release_http_policy::tools::Invocation>>,
}
impl Command {
    pub(super) fn capture(program: Program, args: &[&str]) -> Self {
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
            ops: None,
            source_git: None,
            comparison: None,
            zstd: None,
            load_image: None,
            mdns: None,
            retained_ops: None,
            ops_acquire: None,
            hosted_ops: None,
            ops_runtime: None,
            ops_stage_dispatch: None,
            benchmark: None,
            benchmark_output: None,
            release_checks: None,
            #[cfg(feature = "release-http-policy")]
            release_http: None,
        }
    }
}
