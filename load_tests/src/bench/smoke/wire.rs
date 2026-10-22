//! Only derived loopback endpoints; tests replace this whole I/O boundary.

use super::config::Ports;
use anyhow::{Result, ensure};
use futures_util::{SinkExt, StreamExt};
use simplestchat::{
    ids::RoomId,
    signaling::protocol::{ClientMessage, ServerMessage},
};
use std::time::Duration;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{
        Error, Message,
        client::IntoClientRequest,
        protocol::{CloseFrame, WebSocketConfig, frame::coding::CloseCode},
    },
};

#[derive(Clone, Debug)]
pub(super) enum Event {
    Joined,
    Restarting,
    Closed { code: u16, clean: bool },
    Other,
}
#[async_trait::async_trait]
pub(super) trait Socket: Send {
    async fn join(&mut self, room: &str) -> Result<()>;
    async fn peer_close(&mut self) -> Result<()>;
    async fn next(&mut self) -> Result<Event>;
    async fn finish(self: Box<Self>) -> Result<()>;
}
#[async_trait::async_trait]
pub(super) trait Wire: Send + Sync {
    async fn ready(&self, ports: Ports) -> Result<bool>;
    async fn connect(&self, ports: Ports) -> Result<Box<dyn Socket>>;
}
pub(super) struct NativeWire {
    client: reqwest::Client,
}
impl NativeWire {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .pool_max_idle_per_host(0)
                .timeout(Duration::from_millis(750))
                .build()?,
        })
    }
}
#[async_trait::async_trait]
impl Wire for NativeWire {
    async fn ready(&self, ports: Ports) -> Result<bool> {
        let response = self
            .client
            .get(format!("{}/ready", ports.origin()))
            .send()
            .await;
        let Ok(mut response) = response else {
            return Ok(false);
        };
        if response.status() != reqwest::StatusCode::OK {
            return Ok(false);
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            ensure!(
                chunk.len() <= 4096usize.saturating_sub(bytes.len()),
                "smoke_readiness_limit"
            );
            bytes.extend_from_slice(&chunk);
        }
        ready_body(&bytes)
    }
    async fn connect(&self, ports: Ports) -> Result<Box<dyn Socket>> {
        let mut request = ports.websocket().into_client_request()?;
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "simplestchat".parse()?);
        let config = WebSocketConfig::default()
            .read_buffer_size(4096)
            .write_buffer_size(4096)
            .max_write_buffer_size(131072)
            .max_message_size(Some(65536))
            .max_frame_size(Some(65536));
        let (stream, response) =
            tokio_tungstenite::connect_async_with_config(request, Some(config), true).await?;
        let socket = NativeSocket {
            stream: Some(stream),
            closed: None,
            frames: 0,
        };
        if response
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|v| v.to_str().ok())
            != Some("simplestchat")
        {
            let _ = Box::new(socket).finish().await;
            anyhow::bail!("smoke_websocket_protocol");
        }
        Ok(Box::new(socket))
    }
}
pub(super) fn ready_body(bytes: &[u8]) -> Result<bool> {
    ensure!(bytes.len() <= 4096, "smoke_readiness_limit");
    let body: serde_json::Value = serde_json::from_slice(bytes)?;
    ensure!(
        body.get("status").and_then(|v| v.as_str()) == Some("ready"),
        "smoke_readiness_body"
    );
    Ok(true)
}
pub(super) fn message(bytes: &[u8]) -> Result<Event> {
    ensure!(bytes.len() <= 65536, "smoke_websocket_limit");
    let value: ServerMessage = serde_json::from_slice(bytes)?;
    match value {
        ServerMessage::RoomJoined { .. } => Ok(Event::Joined),
        ServerMessage::ServerRestarting { reason } if reason == "Server shutting down" => {
            Ok(Event::Restarting)
        }
        ServerMessage::ServerRestarting { .. }
        | ServerMessage::RoomClosed { .. }
        | ServerMessage::Error { .. }
        | ServerMessage::RoomPasswordRequired
        | ServerMessage::LobbyWaiting { .. } => anyhow::bail!("smoke_guest_admission_or_lifecycle"),
        _ => Ok(Event::Other),
    }
}
type Stream = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
struct NativeSocket {
    stream: Option<Stream>,
    closed: Option<u16>,
    frames: usize,
}
impl NativeSocket {
    fn stream(&mut self) -> &mut Stream {
        self.stream.as_mut().unwrap()
    }
    fn close_tcp(&mut self) -> Result<()> {
        if let Some(stream) = self.stream.take() {
            match stream.into_inner() {
                MaybeTlsStream::Plain(stream) => nix::unistd::close(stream.into_std()?)?,
                _ => anyhow::bail!("smoke_non_loopback_transport"),
            }
        }
        Ok(())
    }
}
#[async_trait::async_trait]
impl Socket for NativeSocket {
    async fn join(&mut self, room: &str) -> Result<()> {
        let value = ClientMessage::JoinRoom {
            room_id: RoomId::new(room)?,
            participant_name: "Shutdown smoke".into(),
            password: None,
            chat_style: None,
        };
        let mut value = serde_json::to_value(value)?;
        // These optional fields were absent in the retained guest request.
        value.as_object_mut().unwrap().retain(|_, v| !v.is_null());
        self.stream()
            .send(Message::Text(serde_json::to_string(&value)?.into()))
            .await?;
        Ok(())
    }
    async fn peer_close(&mut self) -> Result<()> {
        self.stream()
            .send(Message::Close(Some(CloseFrame {
                code: CloseCode::Normal,
                reason: "Peer close smoke".into(),
            })))
            .await?;
        Ok(())
    }
    async fn next(&mut self) -> Result<Event> {
        loop {
            self.frames += 1;
            ensure!(self.frames <= 128, "smoke_websocket_frames");
            match self.stream().next().await {
                Some(Ok(Message::Text(text))) => return message(text.as_bytes()),
                Some(Ok(Message::Ping(_) | Message::Pong(_))) => {
                    self.stream().flush().await?;
                }
                Some(Ok(Message::Close(frame))) => {
                    self.closed = frame.map(|v| u16::from(v.code));
                    match self.stream().flush().await {
                        Ok(()) | Err(Error::ConnectionClosed) => {}
                        Err(error) => return Err(error.into()),
                    }
                    // A close frame alone is insufficient: finish queued reply
                    // and observe protocol termination/EOF before clean=true.
                }
                None | Some(Err(Error::ConnectionClosed)) => {
                    return self
                        .closed
                        .map(|code| Event::Closed { code, clean: true })
                        .ok_or_else(|| anyhow::anyhow!("smoke_websocket_eof_without_close"));
                }
                Some(Err(error)) => return Err(error.into()),
                Some(Ok(_)) => anyhow::bail!("smoke_websocket_frame"),
            }
        }
    }
    async fn finish(mut self: Box<Self>) -> Result<()> {
        let work = async {
            if self.closed.is_none() {
                self.stream().close(None).await?;
                while !matches!(self.next().await?, Event::Closed { .. }) {}
            }
            Ok::<_, anyhow::Error>(())
        };
        let result = tokio::time::timeout(Duration::from_secs(2), work)
            .await
            .map_err(|_| anyhow::anyhow!("smoke_socket_close_timeout"))
            .and_then(|v| v);
        let closed = self.close_tcp();
        result.and(closed)
    }
}
impl Drop for NativeSocket {
    fn drop(&mut self) {
        let _ = self.close_tcp();
    }
}
