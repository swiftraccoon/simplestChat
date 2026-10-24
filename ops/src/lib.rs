//! Release/artifact policy and explicitly requested private capacity operations.
//! Importing a module starts no process, engine, connection or workload. The
//! capacity routes retain their separate host, provenance and ownership gates;
//! the default graph requires no server, media or native TLS implementation.
//!
//! Validated metadata is not signature evidence or authorization to deploy. Python
//! remains the release runtime until independent parity and distribution checks.

pub mod capacity_vps;
pub mod distribution;
mod files;
mod json;
pub mod release;

/// Fixed, non-sensitive validation failures. Paths, keys and file contents never
/// become error text; callers may retain private evidence separately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Policy(release::trust::Failure),
    Input,
    Changed,
    Json,
    AmbiguousJson,
    Fields,
    Schema,
    Revision,
    Platform,
    ArchiveDigest,
    Tag,
    Migrations,
    MigrationVersion,
    MigrationDigest,
    TimestampShape,
    Timestamp,
}
impl Error {
    pub fn code(self) -> &'static str {
        match self {
            Self::Policy(failure) => failure.code(),
            Self::Input => "manifest_input_refused",
            Self::Changed => "manifest_input_changed",
            Self::Json => "invalid_json",
            Self::AmbiguousJson => "ambiguous_json",
            Self::Fields => "release_manifest_fields",
            Self::Schema => "release_schema",
            Self::Revision => "release_revision",
            Self::Platform => "release_platform",
            Self::ArchiveDigest => "archive_digest",
            Self::Tag => "release_tag",
            Self::Migrations => "release_migrations",
            Self::MigrationVersion => "migration_version",
            Self::MigrationDigest => "migration_digest",
            Self::TimestampShape => "release_timestamp_shape",
            Self::Timestamp => "release_timestamp",
        }
    }
}
impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code())
    }
}
impl std::error::Error for Error {}

#[cfg(test)]
mod test_support;
