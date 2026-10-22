use super::{
    config::Config,
    files::Artifacts,
    io::{Chat, Frame, Resource, Response, Result, Socket, Wire, fail},
    report::{self, State},
};
use serde_json::{json, value::RawValue};
use std::{cell::Cell, collections::BTreeMap, future::Future, time::Duration};
use tokio::time::Instant;

pub(super) trait Clock {
    fn elapsed(&self) -> f64;
    fn utc(&self) -> String;
}
pub(super) trait Cancellation {
    async fn pending(&mut self) -> Option<u8>;
    async fn next(&mut self) -> u8;
}
pub(super) struct Services<'a, W, C, S> {
    pub wire: &'a W,
    pub clock: &'a C,
    pub signals: &'a mut S,
}
struct Timing<'a, C> {
    clock: &'a C,
    last: Cell<f64>,
}
impl<C: Clock> Timing<'_, C> {
    fn now(&self) -> Result<f64> {
        let value = self.clock.elapsed();
        if !value.is_finite() || value < self.last.get() || value < 0.0 {
            return Err(fail(1, "invalid_clock"));
        }
        self.last.set(value);
        Ok(value)
    }
    fn deadline(&self, limit: f64) -> Result<Instant> {
        let remaining = limit - self.now()?;
        if remaining <= 0.0 {
            return Err(fail(
                124,
                if limit == 35.0 {
                    "work_deadline"
                } else {
                    "total_deadline"
                },
            ));
        }
        Ok(Instant::now() + Duration::from_secs_f64(remaining))
    }
    async fn check<S: Cancellation>(&self, signals: &mut S, limit: f64) -> Result<()> {
        if let Some(status) = signals.pending().await {
            return Err(fail(status, "interrupted"));
        }
        self.deadline(limit).map(|_| ())
    }
    async fn wait<T, S: Cancellation>(
        &self,
        signals: &mut S,
        future: impl Future<Output = Result<T>>,
        post: bool,
    ) -> Result<T> {
        self.check(signals, 35.0).await?;
        let deadline = self.deadline(35.0)?;
        let value = tokio::select! {
            biased;
            status = signals.next() => return Err(fail(status, "interrupted")),
            result = future => result?,
            _ = tokio::time::sleep_until(deadline) => return Err(fail(124, "work_deadline")),
        };
        if post {
            self.check(signals, 35.0).await?;
        }
        Ok(value)
    }
}
struct Guest<S> {
    socket: S,
    participant: Option<String>,
    left: bool,
}
async fn request<W: Wire, C: Clock, S: Cancellation>(
    services: &mut Services<'_, W, C, S>,
    timing: &Timing<'_, C>,
    path: &str,
    expected: u16,
    limit: usize,
    accept: &'static str,
) -> Result<Response> {
    let resource = Resource {
        path: path.into(),
        expected,
        accept,
        limit,
    };
    let deadline = timing.deadline(35.0)?;
    let response = timing
        .wait(
            services.signals,
            services.wire.request(&resource, deadline),
            true,
        )
        .await?;
    if response.status != expected {
        return Err(fail(1, "unexpected_http_status"));
    }
    if accept != "*/*"
        && !response
            .headers
            .get("content-type")
            .is_some_and(|s| s.to_lowercase().starts_with(accept))
    {
        return Err(fail(1, "unexpected_content_type"));
    }
    if response.body.len() > limit {
        return Err(fail(1, "response_too_large"));
    }
    if response.headers.get("content-encoding").is_some_and(|s| {
        !s.split(',')
            .all(|s| s.trim().eq_ignore_ascii_case("identity"))
    }) {
        return Err(fail(1, "unsupported_content_encoding"));
    }
    Ok(response)
}
async fn send<S: Socket, C: Clock, X: Cancellation>(
    guest: &mut Guest<S>,
    value: serde_json::Value,
    timing: &Timing<'_, C>,
    signals: &mut X,
) -> Result<()> {
    let text = serde_json::to_string(&value).map_err(|_| fail(1, "operation_failed"))?;
    let deadline = timing.deadline(35.0)?;
    timing
        .wait(signals, guest.socket.send(&text, deadline), false)
        .await
}
async fn flush<S: Socket, C: Clock, X: Cancellation>(
    guest: &mut Guest<S>,
    timing: &Timing<'_, C>,
    signals: &mut X,
) -> Result<()> {
    let deadline = timing.deadline(35.0)?;
    timing
        .wait(signals, guest.socket.flush(deadline), true)
        .await
}
async fn next<S: Socket, C: Clock, X: Cancellation>(
    guests: &mut [Guest<S>],
    timing: &Timing<'_, C>,
    signals: &mut X,
) -> Result<(usize, Frame)> {
    let deadline = timing.deadline(35.0)?;
    let received = async {
        if guests.len() == 1 {
            return guests[0].socket.next(deadline).await.map(|f| (0, f));
        }
        let (a, b) = guests.split_at_mut(1);
        tokio::select! {
            result = a[0].socket.next(deadline) => result.map(|f| (0,f)),
            result = b[0].socket.next(deadline) => result.map(|f| (1,f)),
        }
    };
    let (index, frame) = timing.wait(signals, received, true).await?;
    if frame.rejected() {
        return Err(fail(1, "room_request_rejected"));
    }
    Ok((index, frame))
}
fn matching(chat: &Chat, client: &str, participant: &str, content: &str) -> bool {
    chat.client.as_deref() == Some(client)
        && chat.participant.as_deref() == Some(participant)
        && chat.content.as_deref() == Some(content)
        && chat.message_id.as_ref().is_some_and(|v| !v.is_empty())
}
async fn body<W: Wire, C: Clock, S: Cancellation>(
    cfg: &Config,
    services: &mut Services<'_, W, C, S>,
    timing: &Timing<'_, C>,
    state: &mut State,
    stage: &mut &'static str,
    guests: &mut Vec<Guest<W::Socket>>,
) -> Result<()> {
    if cfg.tls_disabled {
        return Err(fail(1, "tls_verification_disabled"));
    }
    for (path, expected) in [("/health", "ok"), ("/ready", "ready")] {
        *stage = if path == "/health" { "health" } else { "ready" };
        let response = request(services, timing, path, 200, 4096, "application/json").await?;
        let text = String::from_utf8_lossy(&response.body);
        let raw: Box<RawValue> =
            serde_json::from_str(&text).map_err(|_| fail(1, "invalid_status_json"))?;
        let fields: BTreeMap<String, Box<RawValue>> =
            serde_json::from_str(raw.get()).map_err(|_| fail(1, "unexpected_service_status"))?;
        let status = fields
            .get("status")
            .and_then(|v| serde_json::from_str::<String>(v.get()).ok());
        if status.as_deref() != Some(expected) {
            return Err(fail(1, "unexpected_service_status"));
        }
        state.checks.insert(*stage, true);
    }
    *stage = "homepage";
    let home = request(services, timing, "/", 200, 128 * 1024, "text/html").await?;
    if !report::security(&home.headers) {
        return Err(fail(1, "missing_security_headers"));
    }
    state.checks.insert("homepage", true);
    state.checks.insert("securityHeaders", true);
    *stage = "assets";
    let assets = report::assets(&String::from_utf8_lossy(&home.body))?;
    drop(home);
    for (path, kind) in assets {
        let response = request(services, timing, &path, 200, 2 * 1024 * 1024, "*/*").await?;
        let media = response
            .headers
            .get("content-type")
            .map(|s| report::trim(s.split(';').next().unwrap_or("")).to_lowercase());
        if !media.as_deref().is_some_and(|v| {
            if kind == "css" {
                v == "text/css"
            } else {
                matches!(v, "text/javascript" | "application/javascript")
            }
        }) {
            return Err(fail(1, "unexpected_asset_type"));
        }
        if report::trim(&String::from_utf8_lossy(&response.body)).is_empty() {
            return Err(fail(1, "empty_asset"));
        }
    }
    state.checks.insert("staticAssets", true);
    *stage = "private_endpoints";
    for path in ["/metrics", "/diagnostics", "/diagnostics/media"] {
        request(services, timing, path, 404, 4096, "*/*").await?;
    }
    state.checks.insert("privateEndpoints", true);
    *stage = "guest_join";
    for index in 0..2 {
        let deadline = timing.deadline(35.0)?;
        let socket = timing
            .wait(services.signals, services.wire.connect(deadline), false)
            .await?;
        // A completed acquisition enters the retained owner before late fences.
        guests.push(Guest {
            socket,
            participant: None,
            left: false,
        });
        timing.check(services.signals, 35.0).await?;
        send(
            &mut guests[index],
            json!({"type":"joinRoom","roomId":cfg.room,
            "participantName":format!("Smoke {} {}", &state.run_id[..8], index+1)}),
            timing,
            services.signals,
        )
        .await?;
        flush(&mut guests[index], timing, services.signals).await?;
        loop {
            let (from, frame) = next(guests, timing, services.signals).await?;
            if from != index || frame.kind != "roomJoined" {
                continue;
            }
            let id = frame
                .participant
                .filter(|v| !v.is_empty())
                .ok_or_else(|| fail(1, "invalid_join_response"))?;
            guests[index].participant = Some(id);
            state.joined += 1;
            break;
        }
    }
    if guests[0].participant == guests[1].participant {
        return Err(fail(1, "duplicate_guest_identity"));
    }
    state.checks.insert("guestJoins", true);
    *stage = "text_delivery";
    let client = format!("smoke-{}", state.run_id);
    let content = format!(
        "[SimplestChat public smoke {}] HTTPS and text delivery check.",
        state.run_id
    );
    let participant = guests[0].participant.clone().unwrap();
    send(
        &mut guests[0],
        json!({"type":"chatMessage","content":content,
        "clientMessageId":client}),
        timing,
        services.signals,
    )
    .await?;
    state.sent += 1;
    flush(&mut guests[0], timing, services.signals).await?;
    timing.check(services.signals, 35.0).await?;
    let mut ack = None;
    let mut delivered = None;
    while ack.is_none() || delivered.is_none() {
        let (from, frame) = next(guests, timing, services.signals).await?;
        if ack.is_none()
            && from == 0
            && frame.kind == "messageAck"
            && frame.client.as_deref() == Some(&client)
            && frame
                .message
                .as_ref()
                .is_some_and(|c| matching(c, &client, &participant, &content))
        {
            ack = frame.message.and_then(|c| c.message_id);
        }
        if delivered.is_none() && from == 1 && frame.kind == "chatReceived" {
            let chat = Chat {
                participant: frame.participant,
                client: frame.client,
                content: frame.content,
                message_id: frame.message_id,
            };
            if matching(&chat, &client, &participant, &content) {
                delivered = chat.message_id;
            }
        }
    }
    if ack != delivered {
        return Err(fail(1, "chat_identity_mismatch"));
    }
    state.checks.insert("textAcknowledgedAndDelivered", true);
    *stage = "explicit_leave";
    send(
        &mut guests[0],
        json!({"type":"leaveRoom"}),
        timing,
        services.signals,
    )
    .await?;
    guests[0].left = true;
    state.leaves += 1;
    flush(&mut guests[0], timing, services.signals).await?;
    loop {
        let (from, frame) = next(guests, timing, services.signals).await?;
        if from == 1
            && frame.kind == "participantLeft"
            && frame.participant.as_deref() == Some(&participant)
        {
            break;
        }
    }
    state.checks.insert("firstLeaveObserved", true);
    send(
        &mut guests[1],
        json!({"type":"leaveRoom"}),
        timing,
        services.signals,
    )
    .await?;
    guests[1].left = true;
    state.leaves += 1;
    flush(&mut guests[1], timing, services.signals).await?;
    timing.check(services.signals, 35.0).await
}
pub(super) struct Outcome {
    pub status: u8,
    pub summary: serde_json::Value,
}
pub(super) async fn run<W: Wire, C: Clock, S: Cancellation, A: Artifacts>(
    cfg: &Config,
    services: &mut Services<'_, W, C, S>,
    mut output: A,
    run_id: String,
) -> Outcome {
    let timing = Timing {
        clock: services.clock,
        last: Cell::new(0.0),
    };
    let mut state = State::new(cfg.origin(), cfg.room.clone(), run_id, services.clock.utc());
    let mut guests = Vec::new();
    let mut stage = "configuration";
    if let Err(error) = body(cfg, services, &timing, &mut state, &mut stage, &mut guests).await {
        state.failed(stage, error);
    }
    if let Err(error) = timing.check(services.signals, 45.0).await {
        state.failed(stage, error);
    }
    let cleanup_start = timing.now();
    if let Err(error) = cleanup_start {
        state.failed("cleanup", error);
    }
    let remaining = cleanup_start.map_or(0.0, |start| (45.0 - start).clamp(0.0, 5.0));
    let deadline = Instant::now() + Duration::from_secs_f64(remaining);
    let cleanup = |mut guest: Guest<W::Socket>| async move {
        let mut leave = false;
        let mut error = None;
        if guest.participant.is_some() && !guest.left {
            match guest
                .socket
                .send("{\"type\":\"leaveRoom\"}", deadline)
                .await
            {
                Ok(()) => {
                    leave = true;
                    if let Err(e) = guest.socket.flush(deadline).await {
                        error = Some(e);
                    }
                }
                Err(e) => error = Some(e),
            }
        }
        let closed = guest.socket.close(deadline).await;
        (leave, error, closed)
    };
    let finalizers = async {
        match guests.len() {
            2 => {
                let b = guests.pop().unwrap();
                let a = guests.pop().unwrap();
                let (a, b) = tokio::join!(cleanup(a), cleanup(b));
                vec![a, b]
            }
            1 => vec![cleanup(guests.pop().unwrap()).await],
            _ => Vec::new(),
        }
    };
    tokio::pin!(finalizers);
    let results = tokio::select! {
        biased;
        status = services.signals.next() => {
            state.failed("cleanup", fail(status, "interrupted"));
            finalizers.await
        },
        results = &mut finalizers => results,
        _ = tokio::time::sleep_until(deadline) => {
            state.failed("cleanup", fail(124, "cleanup_deadline"));
            finalizers.await
        },
    };
    // Returned errors are already observed. Preserve them before sampling a
    // later clock, while positive close acceptance still awaits that sample.
    for (_, leave, closed) in &results {
        if let Some(error) = leave {
            state.failed("cleanup", fail(error.status, "leave_failed"));
        }
        match closed {
            Err(error) => state.failed("cleanup", *error),
            Ok(value) if !value.clean => {
                state.failed("cleanup", fail(1, "unclean_websocket_shutdown"));
            }
            _ => {}
        }
    }
    let cleanup_end = timing.now();
    if let Err(error) = cleanup_end {
        state.failed("cleanup", error);
    }
    let admitted_clean = matches!((cleanup_start, cleanup_end),
        (Ok(start), Ok(end)) if end-start < 5.0 && end < 45.0);
    for (left, error, closed) in results {
        state.leaves += usize::from(left);
        if let Some(e) = error {
            state.failed("cleanup", fail(e.status, "leave_failed"));
        }
        match closed {
            Ok(v) if v.clean && admitted_clean => state.closed += 1,
            Ok(v) if v.clean => state.failed("cleanup", fail(124, "cleanup_deadline")),
            Ok(_) => state.failed("cleanup", fail(1, "unclean_websocket_shutdown")),
            Err(e) => state.failed("cleanup", e),
        }
    }
    match (cleanup_start, cleanup_end) {
        (Ok(start), Ok(end)) if end - start < 5.0 => {}
        (Err(e), _) | (_, Err(e)) => state.failed("cleanup", e),
        _ => state.failed("cleanup", fail(124, "cleanup_deadline")),
    }
    if state.closed == 2 {
        state.checks.insert("webSocketCleanup", true);
    } else {
        state.failed("cleanup", fail(1, "unclean_websocket_shutdown"));
    }
    if let Err(e) = timing.check(services.signals, 45.0).await {
        state.failed("cleanup", e);
    }
    state.completed = true;
    let elapsed = match timing.now() {
        Ok(value) => {
            if value >= 45.0 {
                state.failed("cleanup", fail(124, "total_deadline"));
            }
            value
        }
        Err(error) => {
            state.failed("cleanup", error);
            0.0
        }
    };
    let summary = state.summary(elapsed);
    let mut status = state.failure.map_or(0, |(_, e)| e.status);
    for result in [output.publish(&summary), output.finish()] {
        if let Err(e) = result
            && status == 0
        {
            status = e.status;
        }
    }
    if let Err(e) = timing.check(services.signals, 45.0).await
        && status == 0
    {
        status = e.status;
    }
    Outcome { status, summary }
}
