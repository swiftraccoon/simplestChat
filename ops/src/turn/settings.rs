//! The relay's prepared identity: its hostname, its pinned image, the digest
//! of its configuration and the secret it shares with the application.

use super::store::{Site, relay_environment};
use crate::host::fault::{
    DIRECTORY, Result, VALUE, decode, digest, field, file, protected, require, text, utf8,
};
use crate::host::hexadecimal;
use crate::json::Json;
use crate::text::python_lines;

const IMAGE: &str = "docker.io/coturn/coturn:4.18.0@sha256:";

/// The prepared identity, read without returning the secret.
pub(super) struct Selected {
    pub domain: String,
    pub image: String,
}

pub(super) fn settings(site: &Site<'_>) -> Result<Selected> {
    let (config, owner) = (&site.host.paths.relay, site.host.owner);
    protected(owner, config, DIRECTORY)?;
    let path = config.join("settings.json");
    protected(owner, &path, file(0o600, 4096))?;
    let record = decode(&std::fs::read(&path)?)?;
    record.object().ok_or(VALUE)?;
    let domain = text(field(&record, "domain")?)?.to_owned();
    // The same boundary the application's own settings draw around the URL.
    relay_environment(&domain, &"0".repeat(64))?;
    let image = text(field(&record, "image")?)?.to_owned();
    let pinned = image.strip_prefix(IMAGE);
    let pinned = pinned.is_some_and(|hash| hexadecimal(hash, 64));
    require(pinned, "Unexpected relay image selection")?;
    let prepared = config.join("turnserver.conf");
    protected(owner, &prepared, file(0o440, 16384))?;
    let configuration = std::fs::read(&prepared)?;
    let recorded = record.get("configurationSha256").and_then(Json::string);
    let unchanged = recorded == Some(digest(&configuration).as_str());
    require(
        unchanged,
        "Relay configuration differs from its prepared identity",
    )?;
    let shared = config.join("secret");
    protected(owner, &shared, file(0o600, 64))?;
    let secret = utf8(std::fs::read(&shared)?)?;
    relay_environment(&domain, &secret)?;
    let configuration = utf8(configuration)?;
    let lines = python_lines(&configuration);
    let secrets: Vec<&str> = lines
        .filter(|line| line.starts_with("static-auth-secret="))
        .collect();
    let same = secrets == [format!("static-auth-secret={secret}")];
    require(same, "Prepared relay and application secrets differ")?;
    Ok(Selected { domain, image })
}
