//! A narrow original ASCII grant policy, not a general URL implementation.
//! The raw request target remains opaque for curl's verbatim HTTP target role.

use super::super::control::{self, Cause, Owner, Result, Window};

pub(super) const MAX_URL: usize = 12288;
pub(super) const MAX_TOKEN: usize = 4096;
pub(super) struct Grant {
    host: String,
    target: String,
}
impl Grant {
    pub fn decode(value: &str, owner: &mut Owner) -> Result<Self> {
        owner.check(Window::Body)?;
        let accepted = !value.is_empty()
            && value.len() <= MAX_URL
            && value.bytes().all(|byte| (33..127).contains(&byte));
        if !accepted {
            return control::failed(owner, Cause::Policy);
        }
        let scheme = value.get(..8).ok_or(Cause::Policy)?;

        let transport = value.strip_suffix('#').unwrap_or(value);
        if !scheme.eq_ignore_ascii_case("https://") || transport.contains('#') {
            return control::failed(owner, Cause::Policy);
        }
        let rest = transport.get(8..).ok_or(Cause::Policy)?;
        let (host, path) = rest.split_once('/').ok_or(Cause::Policy)?;
        let label = host
            .strip_suffix(".blob.core.windows.net")
            .ok_or(Cause::Policy)?;
        let (path, query) = path.split_once('?').ok_or(Cause::Policy)?;
        if label.is_empty()
            || label.len() > 63
            || !label.as_bytes()[0].is_ascii_lowercase() && !label.as_bytes()[0].is_ascii_digit()
            || !label
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            || query.is_empty()
        {
            return control::failed(owner, Cause::Policy);
        }
        Ok(Self {
            host: host.into(),
            target: format!("/{path}?{query}"),
        })
    }
    pub fn host(&self) -> &str {
        &self.host
    }
    pub fn target(&self) -> &str {
        &self.target
    }
}
pub(super) fn token(bytes: &[u8], owner: &mut Owner) -> Result<String> {
    owner.check(Window::Body)?;
    let value = std::str::from_utf8(bytes)
        .map_err(|_| Cause::Policy)?
        .trim_matches(|ch: char| ch.is_whitespace() || matches!(ch, '\u{1c}'..='\u{1f}'));
    if value.is_empty()
        || value.len() > MAX_TOKEN
        || !value.bytes().all(|byte| (33..127).contains(&byte))
    {
        return control::failed(owner, Cause::Policy);
    }
    Ok(value.into())
}
