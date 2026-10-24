//! Mature HTTP framing plus raw selected field values. No URL or HTTP authority.

use super::super::control::{self, Cause, Owner, Result, Window};

const LINE: usize = 65536;
const COUNT: usize = 100;
const BLOCK: usize = LINE * (COUNT + 1);
#[derive(Default)]
pub(super) struct Decoder {
    pending: Vec<u8>,
    line: usize,
    lines: usize,
    done: bool,
}
pub(super) struct Header {
    pub status: u16,
    fields: Vec<(String, Vec<u8>, bool)>,
}
impl Decoder {
    /// Returns the final response and the consumed prefix of this input chunk.
    /// Only 100 is an interim response in the original HTTPResponse adapter.
    pub fn push(&mut self, bytes: &[u8], owner: &mut Owner) -> Result<Option<(Header, usize)>> {
        if self.done {
            return control::failed(owner, Cause::Policy);
        }
        for (at, byte) in bytes.iter().copied().enumerate() {
            owner.check(Window::Body)?;
            self.line += 1;
            if self.line > LINE || self.pending.len() >= BLOCK {
                return control::failed(owner, Cause::Bounds);
            }
            self.pending.push(byte);
            if byte != b'\n' {
                continue;
            }
            self.lines += 1;
            if self.lines > COUNT + 1 {
                return control::failed(owner, Cause::Bounds);
            }
            let blank =
                self.line == 1 || (self.line == 2 && self.pending[self.pending.len() - 2] == b'\r');
            self.line = 0;
            if !blank {
                continue;
            }
            let header = Header::decode(&self.pending, owner)?;
            self.pending.clear();
            self.lines = 0;
            if header.status == 100 {
                continue;
            }
            self.done = true;
            return Ok(Some((header, at + 1)));
        }
        Ok(None)
    }
}
impl Header {
    fn decode(bytes: &[u8], owner: &mut Owner) -> Result<Self> {
        let mut storage = [httparse::EMPTY_HEADER; COUNT];
        let mut response = httparse::Response::new(&mut storage);
        let parsed = httparse::ParserConfig::default()
            .allow_obsolete_multiline_headers_in_responses(true)
            .parse_response(&mut response, bytes)
            .map_err(|_| Cause::Policy)?;
        if parsed != httparse::Status::Complete(bytes.len()) {
            return control::failed(owner, Cause::Policy);
        }
        let status = response.code.ok_or(Cause::Policy)?;
        let mut fields: Vec<(String, Vec<u8>, bool)> = Vec::new();
        // Framing and field grammar have already been validated by httparse.
        // Raw spans preserve trailing whitespace which httparse normalizes.
        for line in bytes.split_inclusive(|byte| *byte == b'\n').skip(1) {
            let line = line.strip_suffix(b"\n").unwrap_or(line);
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            if line.is_empty() {
                break;
            }
            if matches!(line.first(), Some(b' ' | b'\t')) {
                let Some((_, _, folded)) = fields.last_mut() else {
                    return control::failed(owner, Cause::Policy);
                };
                *folded = true;
                continue;
            }
            let colon = line
                .iter()
                .position(|byte| *byte == b':')
                .ok_or(Cause::Policy)?;
            let name = std::str::from_utf8(&line[..colon]).map_err(|_| Cause::Policy)?;
            let value = &line[colon + 1..];
            let start = value
                .iter()
                .position(|byte| !matches!(byte, b' ' | b'\t'))
                .unwrap_or(value.len());
            fields.push((name.to_ascii_lowercase(), value[start..].to_vec(), false));
        }
        owner.check(Window::Body)?;
        Ok(Self { status, fields })
    }
    pub fn location(&self, owner: &mut Owner) -> Result<String> {
        let selected: Vec<_> = self
            .fields
            .iter()
            .filter(|(name, _, _)| name == "location")
            .collect();
        if self.status != 302 || selected.len() != 1 || selected[0].2 {
            return control::failed(owner, Cause::Policy);
        }
        std::str::from_utf8(&selected[0].1)
            .map(str::to_owned)
            .map_err(|_| Cause::Policy)
    }
    pub fn receiver(&self, expected: u64, owner: &mut Owner) -> Result<()> {
        let first = |key: &str| self.fields.iter().find(|(name, _, _)| name == key);
        let encoding = first("content-encoding");
        let length = first("content-length");
        if self.status != 200
            || encoding.is_some_and(|(_, bytes, folded)| *folded || bytes != b"identity")
            || length.is_some_and(|(_, bytes, folded)| {
                *folded || bytes != expected.to_string().as_bytes()
            })
        {
            return control::failed(owner, Cause::Policy);
        }
        Ok(())
    }
    fn joined(&self, key: &str, owner: &mut Owner) -> Result<Option<Vec<u8>>> {
        let mut value = None;
        for (name, bytes, folded) in &self.fields {
            if name != key {
                continue;
            }
            if *folded {
                return control::failed(owner, Cause::Policy);
            }
            let repeated = value.is_some();
            let target = value.get_or_insert_with(Vec::new);
            if repeated {
                target.extend_from_slice(b", ");
            }
            target.extend_from_slice(bytes);
        }
        Ok(value)
    }
    pub fn blob(&self, expected: u64, owner: &mut Owner) -> Result<()> {
        let encoding = self.joined("content-encoding", owner)?;
        let length = self.joined("content-length", owner)?;
        let expected = expected.to_string();
        if self.status != 200
            || encoding.as_ref().is_some_and(|value| value != b"identity")
            || length
                .as_ref()
                .is_some_and(|value| value != expected.as_bytes())
        {
            return control::failed(owner, Cause::Policy);
        }
        Ok(())
    }
}
