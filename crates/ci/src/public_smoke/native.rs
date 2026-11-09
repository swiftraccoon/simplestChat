//! Mature HTTPS/WSS transport for the closed public-smoke transaction.
//!
//! TLS verification uses the native platform defaults. No request retries,
//! redirects, proxy discovery, cookies, credentials, Origin or subprotocol are
//! added. Header accounting occurs after the mature parser. Client/TLS/DNS
//! internals use their ordinary Drop owners: this is not an independently
//! observed close result for every internal descriptor, task or remote action.
//! A positive WebSocket close requires the actual close handshake and client
//! protocol EOF; a timeout or forced transport drop is always negative.

use super::io::{CloseObservation, Frame, Resource, Response, Result, Socket, Wire, fail};
use futures_util::{Sink, SinkExt, StreamExt};
use reqwest::header::{ACCEPT, ACCEPT_ENCODING, CONTENT_ENCODING, HeaderMap};
use std::{collections::BTreeMap, future::poll_fn, pin::Pin, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::TcpStream,
    time::Instant,
};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{
        Error, Message,
        protocol::{CloseFrame, WebSocketConfig, frame::coding::CloseCode},
    },
};

const HEADER_BYTES: usize = 64 * 1024;
const HEADER_COUNT: usize = 64;
const MESSAGE_BYTES: usize = 256 * 1024;
const MESSAGE_COUNT: usize = 4096;
const AGGREGATE_BYTES: usize = 8 * 1024 * 1024;
const HTTP_BODY_BYTES: usize = 2 * 1024 * 1024;
type Transport = MaybeTlsStream<TcpStream>;

pub(super) struct Native {
    origin: url::Url,
    websocket: url::Url,
    client: reqwest::Client,
}
impl Native {
    pub fn new(origin: &url::Url) -> Result<Self> {
        if origin.scheme() != "https"
            || origin.host_str().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.path() != "/"
            || origin.query().is_some()
            || origin.fragment().is_some()
        {
            return Err(fail(1, "invalid_origin"));
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .referer(false)
            .pool_max_idle_per_host(0)
            .https_only(true)
            .no_gzip()
            .no_brotli()
            .no_deflate()
            .no_zstd()
            .timeout(Duration::from_secs(8))
            .build()
            .map_err(|_| fail(1, "operation_failed"))?;
        let mut websocket = origin.clone();
        websocket
            .set_scheme("wss")
            .map_err(|()| fail(1, "invalid_origin"))?;
        websocket.set_path("/ws");
        Ok(Self {
            origin: origin.clone(),
            websocket,
            client,
        })
    }
    fn resource(&self, resource: &Resource) -> Result<url::Url> {
        if resource.limit == 0
            || resource.limit > HTTP_BODY_BYTES
            || !matches!(resource.accept, "application/json" | "text/html" | "*/*")
            || !resource.path.starts_with('/')
            || resource.path.starts_with("//")
            || resource.path.contains(['?', '#', '\\'])
            || resource.path.chars().any(char::is_control)
        {
            return Err(fail(1, "operation_failed"));
        }
        let url = self
            .origin
            .join(&resource.path)
            .map_err(|_| fail(1, "operation_failed"))?;
        if url.origin() != self.origin.origin() || url.path() != resource.path {
            return Err(fail(1, "operation_failed"));
        }
        Ok(url)
    }
}

fn headers(headers: &HeaderMap) -> Result<BTreeMap<String, String>> {
    if headers.len() > HEADER_COUNT {
        return Err(fail(1, "response_headers_too_large"));
    }
    let mut bytes = 0usize;
    let mut result = BTreeMap::<String, String>::new();
    for (name, value) in headers {
        bytes = bytes
            .checked_add(name.as_str().len())
            .and_then(|v| v.checked_add(value.as_bytes().len()))
            .and_then(|v| v.checked_add(4))
            .filter(|v| *v <= HEADER_BYTES)
            .ok_or_else(|| fail(1, "response_headers_too_large"))?;
        let value = String::from_utf8_lossy(value.as_bytes());
        result
            .entry(name.as_str().into())
            .and_modify(|old| {
                old.push_str(", ");
                old.push_str(&value);
            })
            .or_insert_with(|| value.into_owned());
    }
    Ok(result)
}
fn identity(headers: &HeaderMap) -> Result<()> {
    for value in headers.get_all(CONTENT_ENCODING) {
        let value = value
            .to_str()
            .map_err(|_| fail(1, "unsupported_content_encoding"))?;
        if !value
            .split(',')
            .all(|part| part.trim().eq_ignore_ascii_case("identity"))
        {
            return Err(fail(1, "unsupported_content_encoding"));
        }
    }
    Ok(())
}
fn append(body: &mut Vec<u8>, chunk: &[u8], maximum: usize) -> Result<()> {
    if chunk.len() > maximum.saturating_sub(body.len()) {
        return Err(fail(1, "response_too_large"));
    }
    body.extend_from_slice(chunk);
    Ok(())
}
fn request_error(error: reqwest::Error) -> super::io::Failure {
    if error.is_timeout() {
        fail(124, "request_deadline")
    } else {
        fail(1, "operation_failed")
    }
}
impl Wire for Native {
    type Socket = NativeSocket<Transport>;

    async fn request(&self, resource: &Resource, deadline: Instant) -> Result<Response> {
        if Instant::now() >= deadline {
            return Err(fail(124, "work_deadline"));
        }
        let url = self.resource(resource)?;
        let operation = async {
            let mut response = self
                .client
                .get(url)
                .header(ACCEPT, resource.accept)
                .header(ACCEPT_ENCODING, "identity")
                .send()
                .await
                .map_err(request_error)?;
            let status = response.status().as_u16();
            let projected = headers(response.headers())?;
            if status != resource.expected {
                return Err(fail(1, "unexpected_http_status"));
            }
            if resource.accept != "*/*"
                && !projected
                    .get("content-type")
                    .is_some_and(|value| value.to_ascii_lowercase().starts_with(resource.accept))
            {
                return Err(fail(1, "unexpected_content_type"));
            }
            identity(response.headers())?;
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(request_error)? {
                append(&mut body, &chunk, resource.limit)?;
            }
            Ok(Response {
                status,
                headers: projected,
                body,
            })
        };
        tokio::time::timeout_at(deadline, operation)
            .await
            .map_err(|_| fail(124, "work_deadline"))?
    }
    async fn connect(&self, deadline: Instant) -> Result<Self::Socket> {
        if Instant::now() >= deadline {
            return Err(fail(124, "work_deadline"));
        }
        let configuration = WebSocketConfig::default()
            .read_buffer_size(4096)
            .write_buffer_size(4096)
            .max_write_buffer_size(MESSAGE_BYTES * 2)
            .max_message_size(Some(MESSAGE_BYTES))
            .max_frame_size(Some(MESSAGE_BYTES));
        // The URL conversion supplies only the required WebSocket handshake
        // fields. The native-tls feature chooses the default verified connector.
        let (stream, response) = tokio::time::timeout_at(
            deadline,
            tokio_tungstenite::connect_async_with_config(
                self.websocket.as_str(),
                Some(configuration),
                true,
            ),
        )
        .await
        .map_err(|_| fail(124, "work_deadline"))?
        .map_err(|_| fail(1, "operation_failed"))?;
        let mut socket = NativeSocket::new(stream);
        // An acquired socket must reach the controller's retained owner before
        // any post-acquisition refusal. The first work call reports this error;
        // cleanup can still attempt the original socket's close handshake.
        socket.admission = headers(response.headers()).err();
        Ok(socket)
    }
}

/// Limits count yielded complete messages, including controls, and payload
/// bytes. Continuation frames hidden inside the mature parser are not counted.
#[derive(Default)]
struct Budget {
    messages: usize,
    bytes: usize,
}
impl Budget {
    fn message(&mut self, message: &Message) -> Result<()> {
        let size = match message {
            Message::Close(Some(frame)) => frame.reason.len() + 2,
            _ => message.len(),
        };
        self.messages = self
            .messages
            .checked_add(1)
            .filter(|v| *v <= MESSAGE_COUNT)
            .ok_or_else(|| fail(1, "invalid_websocket_message"))?;
        self.bytes = self
            .bytes
            .checked_add(size)
            .filter(|v| *v <= AGGREGATE_BYTES)
            .ok_or_else(|| fail(1, "invalid_websocket_message"))?;
        Ok(())
    }
}
pub(super) struct NativeSocket<S> {
    stream: WebSocketStream<S>,
    budget: Budget,
    admission: Option<super::io::Failure>,
    peer_close: Option<u16>,
    premature_close: bool,
}
impl<S: AsyncRead + AsyncWrite + Unpin> NativeSocket<S> {
    fn new(stream: WebSocketStream<S>) -> Self {
        Self {
            stream,
            budget: Budget::default(),
            admission: None,
            peer_close: None,
            premature_close: false,
        }
    }
    fn text(message: &[u8]) -> Result<Frame> {
        let frame = Frame::decode(message)?;
        if frame.rejected() {
            return Err(fail(1, "room_request_rejected"));
        }
        Ok(frame)
    }
    async fn flush_owned(&mut self) -> Result<()> {
        match self.stream.flush().await {
            Ok(()) | Err(Error::ConnectionClosed) => Ok(()),
            Err(_) => Err(fail(1, "websocket_failed")),
        }
    }
    async fn close_handshake(&mut self) -> Result<CloseObservation> {
        self.stream
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::Normal,
                reason: "Public smoke complete".into(),
            })))
            .await
            .map_err(|_| fail(1, "websocket_close_failed"))?;
        loop {
            match self.stream.next().await {
                Some(Ok(message)) => {
                    self.budget.message(&message)?;
                    match message {
                        Message::Close(frame) => {
                            self.peer_close = frame.map(|frame| u16::from(frame.code));
                            self.flush_owned().await?;
                        }
                        Message::Ping(_) | Message::Pong(_) => self.flush_owned().await?,
                        Message::Text(text) => {
                            Self::text(text.as_bytes())?;
                        }
                        _ => return Err(fail(1, "invalid_websocket_message")),
                    }
                }
                None | Some(Err(Error::ConnectionClosed)) => break,
                Some(Err(_)) => return Err(fail(1, "websocket_failed")),
            }
        }
        if self.peer_close != Some(1000) || self.premature_close {
            return Err(fail(1, "unclean_websocket_shutdown"));
        }
        Ok(CloseObservation { clean: true })
    }
}
impl<S: AsyncRead + AsyncWrite + Unpin> Socket for NativeSocket<S> {
    async fn send(&mut self, text: &str, deadline: Instant) -> Result<()> {
        if Instant::now() >= deadline {
            return Err(fail(124, "work_deadline"));
        }
        if let Some(error) = self.admission {
            return Err(error);
        }
        if text.len() > MESSAGE_BYTES {
            return Err(fail(1, "invalid_websocket_message"));
        }
        let operation = async {
            poll_fn(|context| Pin::new(&mut self.stream).poll_ready(context))
                .await
                .map_err(|_| fail(1, "websocket_not_open"))?;
            // Return on queue acceptance so the controller records the send
            // before a later flush failure. Cancellation never retries it;
            // flush/next/close retain ownership of any queued bytes.
            Pin::new(&mut self.stream)
                .start_send(Message::Text(text.into()))
                .map_err(|_| fail(1, "websocket_not_open"))
        };
        tokio::time::timeout_at(deadline, operation)
            .await
            .map_err(|_| fail(124, "work_deadline"))?
    }
    async fn flush(&mut self, deadline: Instant) -> Result<()> {
        if Instant::now() >= deadline {
            return Err(fail(124, "work_deadline"));
        }
        if let Some(error) = self.admission {
            return Err(error);
        }
        tokio::time::timeout_at(deadline, self.stream.flush())
            .await
            .map_err(|_| fail(124, "work_deadline"))?
            .map_err(|_| fail(1, "websocket_failed"))
    }
    async fn next(&mut self, deadline: Instant) -> Result<Frame> {
        if Instant::now() >= deadline {
            return Err(fail(124, "work_deadline"));
        }
        if let Some(error) = self.admission {
            return Err(error);
        }
        let operation = async {
            loop {
                let message = match self.stream.next().await {
                    Some(Ok(message)) => message,
                    None | Some(Err(Error::ConnectionClosed)) => {
                        return Err(fail(1, "websocket_closed"));
                    }
                    Some(Err(_)) => return Err(fail(1, "websocket_failed")),
                };
                self.budget.message(&message)?;
                match message {
                    Message::Text(text) => return Self::text(text.as_bytes()),
                    Message::Ping(_) | Message::Pong(_) => self.flush_owned().await?,
                    Message::Close(frame) => {
                        self.peer_close = frame.map(|frame| u16::from(frame.code));
                        self.premature_close = true;
                        self.flush_owned().await?;
                        return Err(fail(1, "websocket_closed"));
                    }
                    _ => return Err(fail(1, "invalid_websocket_message")),
                }
            }
        };
        tokio::time::timeout_at(deadline, operation)
            .await
            .map_err(|_| fail(124, "work_deadline"))?
    }
    async fn close(mut self, deadline: Instant) -> Result<CloseObservation> {
        // Always attempt this finalizer, even after admission/work failure. The
        // controller joins both sockets with this same absolute cleanup limit.
        let result = tokio::time::timeout_at(deadline, self.close_handshake())
            .await
            .map_err(|_| fail(124, "cleanup_deadline"))?;
        if result.is_ok() && Instant::now() >= deadline {
            return Err(fail(124, "cleanup_deadline"));
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncWriteExt, DuplexStream};
    use tokio_tungstenite::tungstenite::protocol::Role;

    async fn pair() -> (NativeSocket<DuplexStream>, WebSocketStream<DuplexStream>) {
        let (client, server) = tokio::io::duplex(1024);
        let client = WebSocketStream::from_raw_socket(client, Role::Client, None).await;
        let server = WebSocketStream::from_raw_socket(server, Role::Server, None).await;
        (NativeSocket::new(client), server)
    }
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(2)
    }

    #[test]
    fn header_cap_and_encoding_checks_preserve_duplicates_and_private_values() {
        let mut values = HeaderMap::new();
        values.append("content-type", "text/html".parse().unwrap());
        values.append("content-type", "other".parse().unwrap());
        assert_eq!(
            headers(&values).unwrap()["content-type"],
            "text/html, other"
        );
        identity(&values).unwrap();
        values.insert(CONTENT_ENCODING, "identity".parse().unwrap());
        identity(&values).unwrap();
        values.append(CONTENT_ENCODING, "gzip".parse().unwrap());
        assert_eq!(
            identity(&values).unwrap_err().code,
            "unsupported_content_encoding"
        );
        values.insert("private-unknown", "x".repeat(HEADER_BYTES).parse().unwrap());
        assert_eq!(
            headers(&values).unwrap_err().code,
            "response_headers_too_large"
        );
        let mut body = b"owned".to_vec();
        assert_eq!(
            append(&mut body, b"overflow", 5).unwrap_err().code,
            "response_too_large"
        );
        assert_eq!(body, b"owned");
        let mut count = HeaderMap::new();
        for _ in 0..HEADER_COUNT {
            count.append("x-owned", "counted".parse().unwrap());
        }
        headers(&count).unwrap();
        count.append("x-owned", "overflow".parse().unwrap());
        assert_eq!(
            headers(&count).unwrap_err().code,
            "response_headers_too_large"
        );
    }

    #[tokio::test]
    async fn in_memory_send_is_queued_once_and_last_wins_frame_projection_is_preserved() {
        let (mut client, mut server) = pair().await;
        client.send("owned once", deadline()).await.unwrap();
        Socket::flush(&mut client, deadline()).await.unwrap();
        assert_eq!(
            server.next().await.unwrap().unwrap().into_text().unwrap(),
            "owned once"
        );
        server
            .send(Message::Text(
                concat!(
                    "{\"type\":\"ignored\",\"type\":\"roomJoined\",",
                    "\"participantId\":\"owned\",\"private\":1e999}"
                )
                .into(),
            ))
            .await
            .unwrap();
        let frame = client.next(deadline()).await.unwrap();
        assert_eq!(frame.kind, "roomJoined");
        assert_eq!(frame.participant.as_deref(), Some("owned"));
    }

    #[tokio::test]
    async fn accepted_send_survives_later_flush_timeout_without_requeueing() {
        let (mut client, mut server) = pair().await;
        client.send("accepted once", deadline()).await.unwrap();
        assert_eq!(
            Socket::flush(&mut client, Instant::now())
                .await
                .unwrap_err(),
            fail(124, "work_deadline")
        );
        let (flushed, received) =
            tokio::join!(Socket::flush(&mut client, deadline()), server.next());
        flushed.unwrap();
        assert_eq!(
            received.unwrap().unwrap().into_text().unwrap(),
            "accepted once"
        );

        let (mut client, server) = pair().await;
        client
            .send("accepted before peer loss", deadline())
            .await
            .unwrap();
        drop(server);
        assert_eq!(
            Socket::flush(&mut client, deadline()).await.unwrap_err(),
            fail(1, "websocket_failed")
        );
    }

    #[tokio::test]
    async fn clean_close_requires_peer_normal_code_and_actual_protocol_end() {
        for code in [CloseCode::Normal, CloseCode::Away] {
            let (client, mut server) = pair().await;
            let server = async move {
                if code == CloseCode::Normal {
                    assert!(matches!(server.next().await, Some(Ok(Message::Close(_)))));
                } else {
                    server
                        .send(Message::Close(Some(CloseFrame {
                            code,
                            reason: "inert".into(),
                        })))
                        .await
                        .unwrap();
                    let _ = server.next().await;
                }
                // Flush the mature parser's queued reply, then release this
                // in-memory transport so the client observes actual EOF.
                let _ = server.flush().await;
            };
            let (closed, ()) = tokio::join!(client.close(deadline()), server);
            if code == CloseCode::Normal {
                assert!(closed.unwrap().clean);
            } else {
                assert!(closed.is_err());
            }
        }
    }

    #[tokio::test]
    async fn abrupt_eof_or_shared_close_deadline_can_never_report_clean() {
        let (client, mut server) = pair().await;
        server.get_mut().shutdown().await.unwrap();
        drop(server);
        assert!(client.close(deadline()).await.is_err());
        let (client, _held_server) = pair().await;
        let result = client.close(Instant::now()).await;
        assert_eq!(result.err().unwrap(), fail(124, "cleanup_deadline"));
    }

    #[test]
    fn complete_message_and_aggregate_budgets_include_control_payloads() {
        let mut count = Budget {
            messages: MESSAGE_COUNT,
            bytes: 0,
        };
        assert!(count.message(&Message::Ping(Vec::new().into())).is_err());
        let mut bytes = Budget {
            messages: 0,
            bytes: AGGREGATE_BYTES - 1,
        };
        assert!(
            bytes
                .message(&Message::Close(Some(CloseFrame {
                    code: CloseCode::Normal,
                    reason: "".into(),
                })))
                .is_err()
        );
    }
}
