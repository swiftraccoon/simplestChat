#![forbid(unsafe_code)]
// Keep the existing public crate name compatible with downstream imports.
#![cfg_attr(
    not(test),
    expect(non_snake_case, reason = "public crate name is simplestChat")
)]

//! Single-process chat and media server. [`signaling`] owns HTTP/WebSocket
//! admission, [`room`] owns authoritative membership and room policy, and
//! [`media`] owns mediasoup workers, routers, and session-scoped transports.
//! [`auth`] and [`db`] provide optional persisted identity and settings.
//! [`shutdown`] coordinates one-way drain; the binary owns runtime deadlines.

pub mod auth;
pub mod db;
pub mod diagnostics;
pub mod media;
pub mod metrics;
pub mod room;
pub mod shutdown;
pub mod signaling;
pub mod turn;

/// Serialized outbound signaling payload. A broadcast serializes once and
/// every recipient's writer sends the same buffer; cloning is a refcount
/// bump and the WebSocket frame is built without copying the text again.
pub type OutboundJson = axum::extract::ws::Utf8Bytes;
