use super::io::{Result, fail};
use std::path::{Component, Path, PathBuf};
use url::Url;

pub(super) struct Config {
    pub origin: Url,
    pub room: String,
    pub output: PathBuf,
    pub tls_disabled: bool,
    pub default_output: bool,
}
impl Config {
    pub fn read(args: &[String], caller: &Path, tls_disabled: bool, id: &str) -> Result<Self> {
        let mut origin = None;
        let mut room = None;
        let mut output = None;
        for pair in args.chunks(2) {
            if pair.len() != 2 || pair[1].is_empty() || pair[1].starts_with("--") {
                return Err(fail(2, "invalid_arguments"));
            }
            let slot = match pair[0].as_str() {
                "--origin" => &mut origin,
                "--room" => &mut room,
                "--output" => &mut output,
                _ => return Err(fail(2, "invalid_arguments")),
            };
            if slot.replace(pair[1].clone()).is_some() {
                return Err(fail(2, "invalid_arguments"));
            }
        }
        let origin = origin.ok_or_else(|| fail(2, "invalid_origin"))?;
        let parsed = Url::parse(&origin).map_err(|_| fail(2, "invalid_origin"))?;
        if origin.chars().any(|c| c.is_whitespace() || c.is_control())
            || origin.contains('\\')
            || !origin.starts_with("https://")
            || parsed.scheme() != "https"
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.path() != "/"
            || parsed.query().is_some()
            || parsed.fragment().is_some()
            || origin[8..].trim_end_matches('/').contains(['/', '?', '#'])
        {
            return Err(fail(2, "invalid_origin"));
        }
        let room = room.unwrap_or_else(|| "lobby".into());
        if room.is_empty()
            || room.len() > 128
            || !room
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(fail(2, "invalid_room"));
        }
        let default_output = output.is_none();
        let output = output
            .map(PathBuf::from)
            .unwrap_or_else(|| caller.join(format!("results/public-smoke.{id}")));
        let Some(text) = output.to_str() else {
            return Err(fail(2, "invalid_output"));
        };
        if !output.is_absolute()
            || text.len() > 4096
            || text.chars().any(char::is_control)
            || output.components().count() > 24
            || output
                .components()
                .any(|v| !matches!(v, Component::RootDir | Component::Normal(_)))
            || output.file_name().is_none()
        {
            return Err(fail(2, "invalid_output"));
        }
        Ok(Self {
            origin: parsed,
            room,
            output,
            tls_disabled,
            default_output,
        })
    }
    pub fn origin(&self) -> String {
        self.origin.origin().ascii_serialization()
    }
}
