//! Private adapter seam. Raw responses never cross the public report boundary.
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use tokio::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Failure {
    pub status: u8,
    pub code: &'static str,
}
pub(super) type Result<T> = std::result::Result<T, Failure>;
pub(super) fn fail(status: u8, code: &'static str) -> Failure {
    Failure { status, code }
}
pub(super) struct Resource {
    pub path: String,
    pub expected: u16,
    pub accept: &'static str,
    pub limit: usize,
}
pub(super) struct Response {
    pub status: u16,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}
pub(super) struct CloseObservation {
    pub clean: bool,
}
pub(super) trait Wire {
    type Socket: Socket;
    async fn request(&self, resource: &Resource, deadline: Instant) -> Result<Response>;
    async fn connect(&self, deadline: Instant) -> Result<Self::Socket>;
}
pub(super) trait Socket {
    async fn send(&mut self, text: &str, deadline: Instant) -> Result<()>;
    async fn flush(&mut self, deadline: Instant) -> Result<()>;
    async fn next(&mut self, deadline: Instant) -> Result<Frame>;
    async fn close(self, deadline: Instant) -> Result<CloseObservation>;
}

#[derive(Clone, Debug, Default)]
pub(super) struct Chat {
    pub participant: Option<String>,
    pub client: Option<String>,
    pub content: Option<String>,
    pub message_id: Option<String>,
}
#[derive(Clone, Debug)]
pub(super) struct Frame {
    pub kind: String,
    pub participant: Option<String>,
    pub client: Option<String>,
    pub content: Option<String>,
    pub message_id: Option<String>,
    pub message: Option<Chat>,
}
type Fields = BTreeMap<String, Box<RawValue>>;
fn string(fields: &Fields, name: &str) -> Option<String> {
    fields
        .get(name)
        .and_then(|value| serde_json::from_str(value.get()).ok())
}
fn chat(fields: &Fields) -> Chat {
    Chat {
        participant: string(fields, "participantId"),
        client: string(fields, "clientMessageId"),
        content: string(fields, "content"),
        message_id: string(fields, "messageId"),
    }
}
impl Frame {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 256 * 1024 {
            return Err(fail(1, "invalid_websocket_message"));
        }
        // BTreeMap's last key wins, matching JSON.parse. Unknown values remain
        // RawValue, so a private large number is never parsed as a Rust float.
        let fields: Fields =
            serde_json::from_slice(bytes).map_err(|_| fail(1, "invalid_websocket_message"))?;
        let kind = string(&fields, "type").ok_or_else(|| fail(1, "invalid_websocket_message"))?;
        let selected = chat(&fields);
        Ok(Self {
            kind,
            participant: selected.participant,
            client: selected.client,
            content: selected.content,
            message_id: selected.message_id,
            message: fields
                .get("message")
                .and_then(|v| serde_json::from_str::<Fields>(v.get()).ok())
                .map(|v| chat(&v)),
        })
    }
    pub fn rejected(&self) -> bool {
        matches!(
            self.kind.as_str(),
            "error" | "socialError" | "roomPasswordRequired" | "roomClosed" | "lobbyWaiting"
        )
    }
}
