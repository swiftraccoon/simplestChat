//! Single-process chat and media server. [`signaling`] owns HTTP/WebSocket
//! admission, [`room`] owns authoritative membership and room policy, and
//! [`media`] owns mediasoup workers, routers, and session-scoped transports.
//! `auth` and `db` provide optional persisted identity and settings.
//! [`shutdown`] coordinates one-way drain; the binary owns runtime deadlines.

#![deny(clippy::unwrap_used, clippy::expect_used)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod app;
pub(crate) mod attachments;
pub(crate) mod auth;
pub mod config;
pub(crate) mod db;
pub(crate) mod diagnostics;
pub mod ids;
pub(crate) mod invite_codes;
pub(crate) mod labels;
pub(crate) mod limits;
pub mod media;
pub(crate) mod metrics;
pub(crate) mod net;
pub(crate) mod push;
pub mod room;
pub(crate) mod saturation;
pub mod secret;
pub mod shutdown;
pub mod signaling;
pub(crate) mod sizing;
pub(crate) mod sync;
pub mod turn;

#[cfg(test)]
mod security_canary_tests;
#[cfg(test)]
mod source_scan;

/// Serialized outbound signaling payload. A broadcast serializes once and
/// every recipient's writer sends the same buffer; cloning is a refcount
/// bump and the WebSocket frame is built without copying the text again.
pub type OutboundJson = axum::extract::ws::Utf8Bytes;
