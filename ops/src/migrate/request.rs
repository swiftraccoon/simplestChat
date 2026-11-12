//! The four fields that bind both machines and one exact release to a move,
//! and the private files a move may read: root's own, of one name and one
//! mode, reached without a link.

use super::value::{field, object_of, text};
use crate::host::failure::{Failure, Result, require};
use crate::host::fault::{object, string};
use crate::host::{Host, domain, hexadecimal, state};
use crate::json::Json;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// The most any private file of a move may hold.
pub(super) const MAX_PRIVATE: u64 = 4 * 1024 * 1024;
const MAX_REQUEST: u64 = 16384;
const MAX_ORIGIN_BYTES: usize = 261;
const FIELDS: [&str; 4] = [
    "operationId",
    "sourceOrigin",
    "destinationOrigin",
    "targetRevision",
];

/// One move.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Request {
    pub operation: String,
    pub source: String,
    pub destination: String,
    pub revision: String,
}

impl Request {
    /// The same four fields the controller and both machines exchange.
    pub(super) fn json(&self) -> Json {
        object(vec![
            ("operationId", string(&self.operation)),
            ("sourceOrigin", string(&self.source)),
            ("destinationOrigin", string(&self.destination)),
            ("targetRevision", string(&self.revision)),
        ])
    }

    /// The one private directory of this move.
    pub(super) fn directory(&self, host: &Host<'_>) -> PathBuf {
        host.paths.root.join("migrations").join(&self.operation)
    }
}

/// The host name of a canonical HTTPS origin: no port, path or credentials.
pub(super) fn site(origin: &str) -> Result<&str> {
    let name = origin.strip_prefix("https://");
    let name = name.filter(|name| origin.len() <= MAX_ORIGIN_BYTES && domain(name));
    name.ok_or(Failure::Refused("invalid_migration_origin"))
}

/// A request of exactly the four fields, each of its one shape.
pub(super) fn parse(value: &Json) -> Result<Request> {
    let entries = value.object().ok_or(Failure::Value)?;
    let exact =
        entries.len() == FIELDS.len() && FIELDS.iter().all(|name| entries.contains_key(*name));
    require(exact, "invalid_migration_request")?;
    let operation = text(field(value, "operationId")?)?;
    let revision = text(field(value, "targetRevision")?)?;
    let source = text(field(value, "sourceOrigin")?)?;
    let destination = text(field(value, "destinationOrigin")?)?;
    require(hexadecimal(operation, 32), "invalid_operation_id")?;
    require(hexadecimal(revision, 40), "invalid_target_revision")?;
    site(source)?;
    site(destination)?;
    Ok(Request {
        operation: operation.to_owned(),
        source: source.to_owned(),
        destination: destination.to_owned(),
        revision: revision.to_owned(),
    })
}

/// Require root's own private directory, or private file of at most `limit`
/// bytes and a single name, reached through no link.
pub(super) fn private(host: &Host<'_>, path: &Path, directory: bool, limit: u64) -> Result<()> {
    let modes = if directory { [0o700] } else { [0o600] };
    state::protected(path, host.owner, directory, &modes, Some(limit))?;
    let direct = std::fs::canonicalize(path)? == path;
    require(direct, "migration_path_symlink")?;
    if !directory {
        let single = std::fs::symlink_metadata(path)?.nlink() == 1;
        require(single, "migration_file_hardlink")?;
    }
    Ok(())
}

/// Whether a name is taken at all, also by a link that leads nowhere.
pub(super) fn taken(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

/// The object in a bounded private file.
pub(super) fn read_object(host: &Host<'_>, path: &Path, limit: u64) -> Result<Json> {
    private(host, path, false, limit)?;
    let value = state::decode(&std::fs::read(path)?)?;
    object_of(&value)?;
    Ok(value)
}

/// The request at its one fixed place below private ancestors.
pub(super) fn request_file(host: &Host<'_>, path: &Path) -> Result<Request> {
    let request = parse(&read_object(host, path, MAX_REQUEST)?)?;
    let directory = request.directory(host);
    require(
        path == directory.join("request.json"),
        "request_path_mismatch",
    )?;
    let (root, config) = (&host.paths.root, &host.paths.config);
    for parent in [root, config, &root.join("migrations"), &directory] {
        private(host, parent, true, MAX_PRIVATE)?;
    }
    Ok(request)
}
