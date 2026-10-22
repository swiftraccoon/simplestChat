//! Fixed loopback routes; register response bodies and cookies are never retained.

use super::{
    engine::{self, Clock, Engine},
    plan::{self, Asset},
};
use crate::{ci::process::Executor, tooling::Result};

pub(super) struct Origin(String);
impl Origin {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let value = engine::line(bytes)?;
        let port = value
            .strip_prefix("127.0.0.1:")
            .ok_or_else(engine::failure)?;
        if port.is_empty()
            || port.len() > 5
            || !port.bytes().all(|byte| byte.is_ascii_digit())
            || port.parse::<u16>().ok().is_none_or(|port| port == 0)
        {
            return Err(engine::failure());
        }
        Ok(Self(format!("http://{value}")))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub(super) async fn fetch<E: Executor, C: Clock>(
    engine: &mut Engine<'_, E, C>,
    origin: &Origin,
    path: &str,
    seconds: u64,
    maximum: u64,
) -> Result<Vec<u8>> {
    engine
        .run(plan::curl(origin.as_str(), path, seconds, maximum))
        .await
}

pub(super) async fn asset<E: Executor, C: Clock>(
    engine: &mut Engine<'_, E, C>,
    origin: &Origin,
    asset: Asset,
    maximum: u64,
) -> Result<Vec<u8>> {
    let mut request = plan::curl(
        origin.as_str(),
        &format!("/{}", asset.name()),
        5,
        maximum + 64 * 1024,
    );
    request
        .arguments
        .insert(request.arguments.len() - 1, "--include".into());
    let bytes = engine.run(request).await?;
    let end = bytes
        .windows(4)
        .position(|value| value == b"\r\n\r\n")
        .filter(|value| *value <= 64 * 1024)
        .ok_or_else(engine::failure)?;
    let header = std::str::from_utf8(&bytes[..end]).map_err(|_| engine::failure())?;
    let mut lines = header.split("\r\n");
    let status = lines.next().ok_or_else(engine::failure)?;
    if status.bytes().any(|byte| !(32..=126).contains(&byte)) {
        return Err(engine::failure());
    }
    if !matches!(
        status
            .split_whitespace()
            .take(2)
            .collect::<Vec<_>>()
            .as_slice(),
        ["HTTP/1.0" | "HTTP/1.1" | "HTTP/2" | "HTTP/3", "200"]
    ) {
        return Err(engine::failure());
    }
    let mut mime = None;
    for line in lines {
        // This smoke admits the image's ordinary ASCII field subset, not
        // obsolete folded fields or arbitrary HTTP header representations.
        if line.starts_with([' ', '\t']) || line.bytes().any(|byte| !(32..=126).contains(&byte)) {
            return Err(engine::failure());
        }
        let (name, value) = line.split_once(':').ok_or_else(engine::failure)?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
        {
            return Err(engine::failure());
        }
        if name.eq_ignore_ascii_case("content-type") {
            if mime.is_some() {
                return Err(engine::failure());
            }
            mime = Some(value.trim_matches([' ', '\t']));
        }
    }
    if !mime.is_some_and(|value| asset.accepts(value.split(';').next().unwrap_or("")))
        || bytes.len() - end - 4 > maximum as usize
    {
        return Err(engine::failure());
    }
    Ok(bytes[end + 4..].to_vec())
}

pub(super) async fn register<E: Executor, C: Clock>(
    engine: &mut Engine<'_, E, C>,
    origin: &Origin,
) -> Result<()> {
    let mut command = plan::curl(origin.as_str(), "/api/auth/register", 15, 0);
    let index = command.arguments.len() - 1;
    command.arguments.splice(
        index..index,
        [
            "--header".into(),
            "Content-Type: application/json".into(),
            "--header".into(),
            format!("Origin: {}", origin.as_str()).into(),
            "--data".into(),
            plan::REGISTER.into(),
            "--output".into(),
            "/dev/null".into(),
        ],
    );
    engine.run(command).await.map(|_| ())
}
