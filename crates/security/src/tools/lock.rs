//! The lock's exact shape. Every field of every tool and platform is checked,
//! selected or not, before any address is requested.

use super::{Fault, Result, digest, files, require};
use crate::record::{MAX_DOCUMENT, bounded, document, record, safe_name, string};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

const MAX_TOOLS: usize = 32;
const MAX_FILES: usize = 10_000;
const PLATFORMS: [&str; 4] = [
    "linux-x86_64",
    "linux-aarch64",
    "darwin-arm64",
    "darwin-x86_64",
];
const RELEASES: &str = "https://github.com";
const PACKAGES: &str = "https://files.pythonhosted.org";

/// How the bytes at an asset's address hold its files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Format {
    TarGz,
    TarXz,
    Binary,
    Zip,
}

/// One reviewed download: where it is, what it is, and which members go where.
#[derive(Debug)]
pub(super) struct Asset {
    pub url: String,
    pub sha256: String,
    pub format: Format,
    /// Archive member to destination below the installation's `bin`.
    pub files: BTreeMap<String, String>,
}

/// A versioned executable and the platforms it is locked for.
#[derive(Debug)]
pub(super) struct Tool {
    pub version: String,
    pub executable: String,
    pub platforms: BTreeMap<String, Asset>,
}

/// The parsed lock and the SHA-256 of its bytes.
#[derive(Debug)]
pub(super) struct Lock {
    pub tools: BTreeMap<String, Tool>,
    pub digest: String,
}

/// An HTTPS release download or package-index wheel, spelled plainly: the
/// transport is handed these bytes, so nothing it could read another way is
/// admitted (no user, port, query, fragment, capitals in scheme or host, space
/// or pattern character).
fn address(url: &str) -> bool {
    let plain = |path: &str| {
        path.starts_with('/')
            && path
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-._~/%+@".contains(&byte))
    };
    let release = url
        .strip_prefix(RELEASES)
        .is_some_and(|path| plain(path) && path.contains("/releases/download/"));
    let wheel = url.strip_prefix(PACKAGES).is_some_and(|path| {
        plain(path) && path.starts_with("/packages/") && path.ends_with(".whl")
    });
    release || wheel
}

fn hexadecimal(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn version(text: &str) -> bool {
    let parts: Vec<&str> = text.split('.').collect();
    parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn tool_name(text: &str) -> bool {
    let mut bytes = text.bytes();
    text.len() <= 64
        && bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

impl Asset {
    fn parse(value: &Value) -> Result<Self> {
        let raw = record(value, &["url", "sha256", "format", "files"])?;
        let Value::Object(listed) = &raw["files"] else {
            return Err(Fault("invalid_tool_files"));
        };
        require(
            !listed.is_empty() && listed.len() <= MAX_FILES,
            "invalid_tool_files",
        )?;
        let mut files = BTreeMap::new();
        for (member, destination) in listed {
            files.insert(bounded(member)?.to_owned(), string(destination)?.to_owned());
        }
        require(
            files
                .iter()
                .all(|(member, destination)| safe_name(member) && safe_name(destination)),
            "invalid_tool_member",
        )?;
        let mut destinations: Vec<&String> = files.values().collect();
        destinations.sort_unstable();
        destinations.dedup();
        require(
            destinations.len() == files.len(),
            "duplicate_tool_destination",
        )?;
        let (url, sha256) = (string(&raw["url"])?, string(&raw["sha256"])?);
        let format = string(&raw["format"])?;
        require(address(url), "invalid_tool_url")?;
        require(hexadecimal(sha256), "invalid_tool_digest")?;
        let format = match format {
            "tar.gz" => Format::TarGz,
            "tar.xz" => Format::TarXz,
            "binary" => Format::Binary,
            "zip" => Format::Zip,
            _ => return Err(Fault("invalid_tool_format")),
        };
        require(
            format != Format::Binary || files.len() == 1,
            "invalid_tool_binary_files",
        )?;
        Ok(Self {
            url: url.to_owned(),
            sha256: sha256.to_owned(),
            format,
            files,
        })
    }
}

impl Tool {
    fn parse(value: &Value) -> Result<Self> {
        let raw = record(value, &["version", "executable", "platforms"])?;
        let (executable, number) = (string(&raw["executable"])?, string(&raw["version"])?);
        require(safe_name(executable), "invalid_tool_executable")?;
        require(version(number), "invalid_tool_version")?;
        let Value::Object(listed) = &raw["platforms"] else {
            return Err(Fault("invalid_tool_platforms"));
        };
        require(
            !listed.is_empty() && listed.keys().all(|name| PLATFORMS.contains(&name.as_str())),
            "invalid_tool_platforms",
        )?;
        let mut platforms = BTreeMap::new();
        for (name, asset) in listed {
            platforms.insert(name.clone(), Asset::parse(asset)?);
        }
        require(
            platforms.values().all(|asset| {
                asset
                    .files
                    .values()
                    .any(|destination| destination == executable)
            }),
            "missing_tool_executable",
        )?;
        Ok(Self {
            version: number.to_owned(),
            executable: executable.to_owned(),
            platforms,
        })
    }
}

impl Lock {
    /// Read and check the whole lock; its digest names the default directory.
    pub(super) fn load(path: &Path) -> Result<Self> {
        let content = files::bounded(path, MAX_DOCUMENT, false)?;
        let value = document(&content)?;
        let raw = record(&value, &["schemaVersion", "tools"])?;
        require(raw["schemaVersion"].as_u64() == Some(1), "tool_lock_schema")?;
        let Value::Object(listed) = &raw["tools"] else {
            return Err(Fault("invalid_tool_set"));
        };
        require(
            !listed.is_empty() && listed.len() <= MAX_TOOLS,
            "invalid_tool_set",
        )?;
        require(
            listed.keys().all(|name| tool_name(name)),
            "invalid_tool_name",
        )?;
        let mut tools = BTreeMap::new();
        for (name, tool) in listed {
            tools.insert(name.clone(), Tool::parse(tool)?);
        }
        Ok(Self {
            tools,
            digest: digest(&content),
        })
    }
}
