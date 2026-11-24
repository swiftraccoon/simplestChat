//! One explicitly requested private capacity operation on the fixed Debian host.
//!
//! Protocol values are data, never authenticated acquisition or execution proofs.
//! The production installer owns the executable at the fixed installed path;
//! every invocation binds its own running image to that file and to the pin
//! its header declares.

#[cfg(target_os = "linux")]
mod actions;
#[cfg(target_os = "linux")]
mod archive;
#[cfg(target_os = "linux")]
mod conclude;
#[cfg(target_os = "linux")]
mod control;
#[cfg(target_os = "linux")]
mod files;
#[cfg(target_os = "linux")]
mod host;
#[cfg(target_os = "linux")]
mod process;
pub mod protocol;
#[cfg(target_os = "linux")]
mod serve;
#[cfg(target_os = "linux")]
mod worker;

#[cfg(all(test, target_os = "linux"))]
mod host_tests;

/// Serve one framed request on the fixed supported private host. The origin
/// of every deadline is this process's own first monotonic sample; the time
/// spent reaching it belongs to the caller's dispatch deadline.
/// Normal results are emitted only after owned peers finish; reserve/archive
/// streams remain negative until their successful process exit and final fences.
pub fn serve(header: &std::ffi::OsStr) -> std::process::ExitCode {
    #[cfg(target_os = "linux")]
    {
        serve::run(header)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = header;
        unsupported()
    }
}

/// Run only the request/hash derived from one canonical systemd capacity unit.
/// No caller path, root, executable or unit override is accepted.
pub fn worker(run: &std::ffi::OsStr, request_sha256: &std::ffi::OsStr) -> std::process::ExitCode {
    #[cfg(target_os = "linux")]
    {
        worker::run(run, request_sha256)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (run, request_sha256);
        unsupported()
    }
}

#[cfg(not(target_os = "linux"))]
fn unsupported() -> std::process::ExitCode {
    use std::io::Write;
    let _ = std::io::stderr().write_all(b"ops_capacity_platform\n");
    std::process::ExitCode::from(1)
}

/// Closed failures; paths, arbitrary JSON and subprocess text remain private.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Arguments,
    Framing,
    Request,
    Identity,
    /// The executable at the fixed installed path is absent or is not the one
    /// the header pins. For a saved run that pin is the run's retained identity,
    /// so its collection and recovery need that artifact installed.
    Installed,
    Changed,
    Namespace,
    Platform,
    HostBusy,
    Source,
    Image,
    Unit,
    Journal,
    Ownership,
    Collection,
    Archive,
    /// A collected run is not proven finished: its unit is not terminal, its
    /// cgroup is not empty, a stray container remains or cleanup is unproven.
    Conclusion,
    Command,
    Bounds,
    Io,
    Close,
    Interrupted,
    Terminated,
    Deadline,
    Unproven,
}

impl Failure {
    /// Fixed public code, containing no supplied text.
    pub fn code(self) -> &'static str {
        match self {
            Self::Arguments => "ops_capacity_arguments",
            Self::Framing => "ops_capacity_framing",
            Self::Request => "ops_capacity_request",
            Self::Identity => "ops_capacity_identity",
            Self::Installed => "ops_capacity_installed",
            Self::Changed => "ops_capacity_changed",
            Self::Namespace => "ops_capacity_namespace",
            Self::Platform => "ops_capacity_platform",
            Self::HostBusy => "ops_capacity_host_busy",
            Self::Source => "ops_capacity_source",
            Self::Image => "ops_capacity_image",
            Self::Unit => "ops_capacity_unit",
            Self::Journal => "ops_capacity_journal",
            Self::Ownership => "ops_capacity_ownership",
            Self::Collection => "ops_capacity_collection",
            Self::Archive => "ops_capacity_archive",
            Self::Conclusion => "ops_capacity_conclusion",
            Self::Command => "ops_capacity_command",
            Self::Bounds => "ops_capacity_bounds",
            Self::Io => "ops_capacity_io",
            Self::Close => "ops_capacity_close",
            Self::Interrupted => "ops_capacity_interrupted",
            Self::Terminated => "ops_capacity_terminated",
            Self::Deadline => "ops_capacity_deadline",
            Self::Unproven => "ops_capacity_unproven",
        }
    }

    /// Recognized owner status, independent of arbitrary child exit integers.
    pub fn exit_code(self) -> u8 {
        match self {
            Self::Arguments => 2,
            Self::Deadline => 124,
            Self::Interrupted => 130,
            Self::Terminated => 143,
            _ => 1,
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for Failure {}

/// Errors preserve a closed code, never an input parser's diagnostic text.
pub type Result<T> = std::result::Result<T, Failure>;
