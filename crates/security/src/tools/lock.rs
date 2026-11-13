//! The lock's exact shape. Every field of every tool and platform is checked,
//! selected or not, before any address is requested.

use super::{Fault, Result, digest, files, require};
use serde_json::{Map, Value};
use std::{collections::BTreeMap, path::Path};

/// The most bytes a lock or a receipt may have.
pub(super) const MAX_DOCUMENT: u64 = 1024 * 1024;
const MAX_STRING: usize = 2048;
const MAX_NAME: usize = 512;
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

/// A JSON document, or the one code for bytes that are none.
pub(super) fn document(content: &[u8]) -> Result<Value> {
    serde_json::from_slice(content).map_err(|_| Fault("invalid_tool_json"))
}

/// An object with exactly `keys`: no field missing and none unknown.
pub(super) fn record<'a>(value: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>> {
    let Value::Object(fields) = value else {
        return Err(Fault("invalid_tool_record"));
    };
    require(
        fields.len() == keys.len() && keys.iter().all(|key| fields.contains_key(*key)),
        "invalid_tool_record_fields",
    )?;
    Ok(fields)
}

fn bounded(text: &str) -> Result<&str> {
    require(
        !text.is_empty() && text.chars().count() <= MAX_STRING,
        "invalid_tool_string",
    )?;
    Ok(text)
}

fn string(value: &Value) -> Result<&str> {
    match value {
        Value::String(text) => bounded(text),
        _ => Err(Fault("invalid_tool_string")),
    }
}

/// A canonical relative path: no other spelling of it is admitted. An empty
/// name is one empty component.
pub(super) fn safe_name(value: &str) -> bool {
    value.chars().count() <= MAX_NAME
        && !value.contains('\\')
        && !value.chars().any(|character| character < ' ')
        && value
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."))
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
