//! Inert observations for the complete controller, not a second verdict policy.
use super::{
    controller::{Cancellation, Clock},
    files::Artifacts,
    io::{CloseObservation, Frame, Resource, Response, Result, Socket, Wire, fail},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    task::Poll,
};
use tokio::time::Instant;

pub(super) const ID: &str = "00000000-0000-4000-8000-000000000001";
const HOME: &str = concat!(
    "<script type=\"module\" src=\"/assets/index-A1.js\"></script>",
    "<link rel=\"stylesheet\" href=\"/assets/index-B2.css\">"
);
pub(super) fn security() -> BTreeMap<String, String> {
    [
        ("strict-transport-security", "max-age=31536000"),
        ("x-content-type-options", "nosniff"),
        ("x-frame-options", "DENY"),
        ("referrer-policy", "no-referrer"),
        ("cross-origin-opener-policy", "same-origin"),
        ("cross-origin-resource-policy", "same-origin"),
        (
            "permissions-policy",
            "camera=(self), microphone=(self), display-capture=(self), geolocation=()",
        ),
        (
            "content-security-policy",
            concat!(
                "default-src 'self'; script-src 'self'; connect-src 'self'; ",
                "form-action 'self'; base-uri 'none'; object-src 'none'; frame-ancestors 'none'"
            ),
        ),
    ]
    .map(|(a, b)| (a.into(), b.into()))
    .into()
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Trace {
    requests: Vec<Value>,
    connects: Vec<String>,
    sent: Vec<Value>,
    closes: Vec<Value>,
    order: Vec<String>,
    cancelled_bodies: usize,
}
pub(super) struct Fake {
    pub options: Value,
    pub clock: Cell<f64>,
    pub signal: Cell<Option<u8>>,
    pub finished: Cell<usize>,
    pub trace: RefCell<Trace>,
    queues: RefCell<Vec<VecDeque<Result<Frame>>>>,
    pub publish: RefCell<Option<Value>>,
    pub artifact_closed: Cell<bool>,
    pub clock_reads: Cell<usize>,
    pub first_close_clock_read: Cell<Option<usize>>,
    cleanup_anchor: Cell<Option<f64>>,
    pub close_started: Cell<usize>,
    pub release_close: tokio::sync::Notify,
    pub signal_wake: tokio::sync::Notify,
    last_chat: Cell<bool>,
}
impl Fake {
    pub fn new(options: &Value) -> Rc<Self> {
        Rc::new(Self {
            options: options.clone(),
            clock: Cell::new(0.0),
            signal: Cell::new(None),
            finished: Cell::new(0),
            trace: RefCell::new(Trace::default()),
            queues: RefCell::new(Vec::new()),
            publish: RefCell::new(None),
            artifact_closed: Cell::new(false),
            clock_reads: Cell::new(0),
            first_close_clock_read: Cell::new(None),
            cleanup_anchor: Cell::new(None),
            close_started: Cell::new(0),
            release_close: tokio::sync::Notify::new(),
            signal_wake: tokio::sync::Notify::new(),
            last_chat: Cell::new(false),
        })
    }
    fn yes(&self, key: &str) -> bool {
        self.options[key].as_bool() == Some(true)
    }
    fn emit(&self, index: usize, value: Value) {
        self.raw(index, &serde_json::to_string(&value).unwrap());
    }
    fn raw(&self, index: usize, value: &str) {
        self.queues.borrow_mut()[index].push_back(Frame::decode(value.as_bytes()));
    }
    pub fn trace(&self) -> Value {
        serde_json::to_value(&*self.trace.borrow()).unwrap()
    }
}
impl Clock for Fake {
    fn elapsed(&self) -> f64 {
        let next = self.clock_reads.get() + 1;
        self.clock_reads.set(next);
        if self.options["invalidAtRead"].as_u64() == Some(next as u64) {
            return self.options["invalidClock"].as_f64().unwrap_or(f64::NAN);
        }
        self.clock.get()
    }
    fn utc(&self) -> String {
        "TIME".into()
    }
}
pub(super) struct Signals(pub Rc<Fake>);
impl Cancellation for Signals {
    async fn pending(&mut self) -> Option<u8> {
        self.0.signal.take()
    }
    async fn next(&mut self) -> u8 {
        if let Some(code) = self.0.signal.take() {
            return code;
        }
        loop {
            self.0.signal_wake.notified().await;
            if let Some(code) = self.0.signal.take() {
                return code;
            }
        }
    }
}
pub(super) struct Output(pub Rc<Fake>);
impl Artifacts for Output {
    fn publish(&mut self, value: &Value) -> Result<()> {
        *self.0.publish.borrow_mut() = Some(value.clone());
        if let Some(code) = self.0.options["publishSignal"].as_u64() {
            self.0.signal.set(Some(code as u8));
        }
        if self.0.yes("publishDeadline") {
            self.0.clock.set(45.0);
        }
        if self.0.yes("publishFailure") {
            return Err(fail(1, "artifact_failed"));
        }
        Ok(())
    }
    fn finish(self) -> Result<()> {
        self.0.artifact_closed.set(true);
        if self.0.yes("artifactCloseFailure") {
            return Err(fail(1, "artifact_failed"));
        }
        Ok(())
    }
}
pub(super) struct Transport(pub Rc<Fake>);
pub(super) struct Guest {
    state: Rc<Fake>,
    index: usize,
}
impl Wire for Transport {
    type Socket = Guest;
    async fn request(&self, r: &Resource, _: Instant) -> Result<Response> {
        let s = &self.0;
        s.trace
            .borrow_mut()
            .requests
            .push(json!({"path":r.path,"accept":r.accept}));
        s.trace
            .borrow_mut()
            .order
            .push(format!("request:{}", r.path));
        let targeted = r.path == s.options["httpPath"].as_str().unwrap_or("/health");
        if targeted && s.yes("fetchFailure") {
            return Err(fail(1, "operation_failed"));
        }
        if targeted && s.yes("fetchDeadline") {
            s.clock.set(8.0);
            return Err(fail(124, "request_deadline"));
        }
        if targeted && s.yes("lateResponse") {
            s.clock.set(35.0);
        }
        if targeted && s.yes("requestCancel") {
            s.signal.set(Some(143));
        }
        let mut headers = if r.path == "/" {
            security()
        } else {
            BTreeMap::new()
        };
        let (mut status, mut media, mut body) = match r.path.as_str() {
            "/health" => (
                200,
                "application/json".to_string(),
                b"{\"status\":\"ok\"}".to_vec(),
            ),
            "/ready" => (
                200,
                "application/json".into(),
                b"{\"status\":\"ready\"}".to_vec(),
            ),
            "/" => (
                200,
                "text/html".into(),
                s.options["html"]
                    .as_str()
                    .unwrap_or(HOME)
                    .as_bytes()
                    .to_vec(),
            ),
            p if p.ends_with(".js") => (200, "text/javascript".into(), b"export {};".to_vec()),
            p if p.ends_with(".css") => (200, "text/css".into(), b"body{}".to_vec()),
            _ => (404, "text/plain".into(), b"not found".to_vec()),
        };
        if targeted {
            if let Some(v) = s.options["httpStatus"].as_u64() {
                status = v as u16;
            }
            if let Some(v) = s.options["httpType"].as_str() {
                media = v.into();
            }
            if let Some(v) = s.options["body"].as_str() {
                body = v.as_bytes().to_vec();
            }
            if let Some(n) = s.options["bodyBytes"].as_u64() {
                if s.yes("padBody") && n >= body.len() as u64 {
                    body.resize(n as usize, b' ');
                } else {
                    body = vec![b'x'; n as usize];
                }
            }
            if let Some(kind) = s.options["invalidUtf8"].as_str() {
                body = if kind == "unknown" {
                    b"{\"status\":\"ok\",\"PRIVATE\":\"\xff\"}".to_vec()
                } else {
                    b"{\"status\":\"o\xffk\"}".to_vec()
                };
            }
        }
        headers.insert("content-type".into(), media.clone());
        if let Some(name) = s.options["missingHeader"].as_str() {
            headers.remove(name);
        }
        if status != r.expected {
            return Err(fail(1, "unexpected_http_status"));
        }
        if r.accept != "*/*" && !media.to_lowercase().starts_with(r.accept) {
            return Err(fail(1, "unexpected_content_type"));
        }
        if targeted && s.yes("identityEncoding") {
            return Err(fail(1, "unsupported_content_encoding"));
        }
        if targeted && s.yes("missingBody") {
            return Err(fail(1, "missing_response_body"));
        }
        s.trace.borrow_mut().cancelled_bodies += 1;
        if body.len() > r.limit {
            return Err(fail(1, "response_too_large"));
        }
        Ok(Response {
            status,
            headers,
            body,
        })
    }
    async fn connect(&self, _: Instant) -> Result<Guest> {
        let index = self.0.queues.borrow().len();
        self.0.queues.borrow_mut().push(VecDeque::new());
        self.0
            .trace
            .borrow_mut()
            .connects
            .push("wss://chat.example.com/ws".into());
        self.0
            .trace
            .borrow_mut()
            .order
            .push(format!("connect:{index}"));
        if self.0.yes("connectLate") {
            self.0.clock.set(35.0);
        }
        if self.0.yes("connectCancel") {
            self.0.signal.set(Some(130));
        }
        Ok(Guest {
            state: self.0.clone(),
            index,
        })
    }
}
fn patched(mut value: Value, patch: &Value) -> Value {
    if let Some(patch) = patch.as_object() {
        for (k, v) in patch {
            value[k] = v.clone();
        }
    }
    value
}
impl Socket for Guest {
    async fn send(&mut self, text: &str, _: Instant) -> Result<()> {
        let s = &self.state;
        let o = &s.options;
        if self.index == 0 && s.yes("socketFailure") {
            return Err(fail(1, "websocket_failed"));
        }
        if self.index == 0 && s.yes("socketClosed") {
            return Err(fail(1, "websocket_closed"));
        }
        let value: Value = serde_json::from_str(text).unwrap();
        let kind = value["type"].as_str().unwrap();
        let row = if kind == "joinRoom" {
            assert_eq!(
                value["participantName"],
                format!("Smoke {} {}", &ID[..8], self.index + 1)
            );
            json!({"guest":self.index,"type":kind,"roomId":value["roomId"]})
        } else {
            json!({"guest":self.index,"type":kind})
        };
        s.trace.borrow_mut().sent.push(row);
        s.trace
            .borrow_mut()
            .order
            .push(format!("send:{}:{kind}", self.index));
        match kind {
            "joinRoom" => {
                if s.yes("joinMissing") {
                    return Ok(());
                }
                if ["binaryFrame", "malformedFrame", "missingType", "largeFrame"]
                    .iter()
                    .any(|k| s.yes(k))
                {
                    s.queues.borrow_mut()[self.index]
                        .push_back(Err(fail(1, "invalid_websocket_message")));
                    return Ok(());
                }
                if let Some(kind) = o["roomRejected"].as_str() {
                    s.emit(self.index, json!({"type":kind}));
                    return Ok(());
                }
                if let Some(raw) = o["rawFrame"].as_str() {
                    s.raw(self.index, raw);
                }
                if o["extraFrames"].as_u64().is_some_and(|v| v >= 128) {
                    s.queues.borrow_mut()[self.index]
                        .push_back(Err(fail(1, "invalid_websocket_message")));
                }
                let participant = if s.yes("emptyParticipant") {
                    "".into()
                } else {
                    format!(
                        "guest-{}",
                        if s.yes("duplicateParticipant") {
                            0
                        } else {
                            self.index
                        }
                    )
                };
                if s.yes("duplicateJoin") {
                    let raw = format!(
                        concat!(
                            "{{\"type\":\"PRIVATE\",\"type\":\"roomJoined\",",
                            "\"participantId\":\"PRIVATE\",\"participantId\":\"{}\",",
                            "\"PRIVATE\":1e300}}"
                        ),
                        participant
                    );
                    s.raw(self.index, &raw);
                } else {
                    s.emit(
                        self.index,
                        json!({"type":"roomJoined","participantId":participant}),
                    );
                }
            }
            "chatMessage" => {
                s.last_chat.set(true);
                assert_eq!(value["clientMessageId"], format!("smoke-{ID}"));
                assert_eq!(
                    value["content"],
                    format!("[SimplestChat public smoke {ID}] HTTPS and text delivery check.")
                );
                if s.yes("sendFailure") {
                    return Err(fail(1, "operation_failed"));
                }
                let entry = json!({
                    "clientMessageId":value["clientMessageId"],"content":value["content"],
                    "participantId":"guest-0","messageId":"message-1"
                });
                let acknowledge = || {
                    if s.yes("ackMissing") {
                        return;
                    }
                    let mut e = patched(entry.clone(), &o["ackPatch"]);
                    if s.yes("ackMismatch") {
                        e["participantId"] = json!("foreign");
                    }
                    let ack = patched(
                        json!({"type":"messageAck", "clientMessageId":value["clientMessageId"],
                        "message":e}),
                        &o["ackOuterPatch"],
                    );
                    s.emit(0, ack);
                    if s.yes("duplicateAck") {
                        let mut e = entry.clone();
                        e["messageId"] = json!("message-2");
                        s.emit(
                            0,
                            json!({
                                "type":"messageAck","clientMessageId":value["clientMessageId"],
                                "message":e
                            }),
                        );
                    }
                };
                if !s.yes("duplicateDelivery") {
                    acknowledge();
                }
                if !s.yes("deliveryMissing") {
                    let mut delivery = entry.clone();
                    delivery["type"] = json!("chatReceived");
                    if s.yes("deliveryMismatch") {
                        delivery["participantId"] = json!("foreign");
                    }
                    if s.yes("chatIdentityMismatch") {
                        delivery["messageId"] = json!("message-2");
                    }
                    s.emit(1, patched(delivery, &o["deliveryPatch"]));
                }
                if s.yes("duplicateDelivery") {
                    let mut e = entry.clone();
                    e["type"] = json!("chatReceived");
                    e["messageId"] = json!("message-2");
                    s.emit(1, e);
                    acknowledge();
                }
            }
            "leaveRoom" => {
                if s.yes("leaveFailure") {
                    return Err(fail(1, "operation_failed"));
                }
                if self.index == 0 && s.queues.borrow().len() == 2 && !s.yes("leaveMissing") {
                    s.emit(
                        1,
                        json!({"type":"participantLeft","participantId":
                        if s.yes("leaveWrong"){ "foreign" }else{ "guest-0" }}),
                    );
                }
            }
            _ => panic!("closed outbound grammar"),
        }
        Ok(())
    }
    async fn flush(&mut self, _: Instant) -> Result<()> {
        if self.state.yes("flushFailure")
            || (self.state.yes("chatFlushFailure") && self.state.last_chat.get())
        {
            return Err(fail(1, "websocket_failed"));
        }
        Ok(())
    }
    async fn next(&mut self, _: Instant) -> Result<Frame> {
        std::future::poll_fn(|_| {
            let mut queues = self.state.queues.borrow_mut();
            if let Some(value) = queues[self.index].pop_front() {
                return Poll::Ready(value);
            }
            if queues.iter().any(|q| !q.is_empty()) {
                return Poll::Pending;
            }
            self.state.clock.set(35.0);
            Poll::Ready(Err(fail(124, "work_deadline")))
        })
        .await
    }
    async fn close(self, deadline: Instant) -> Result<CloseObservation> {
        let s = &self.state;
        if s.first_close_clock_read.get().is_none() {
            s.first_close_clock_read.set(Some(s.clock_reads.get()));
        }
        s.close_started.set(s.close_started.get() + 1);
        if s.yes("holdClose") {
            s.release_close.notified().await;
        }
        s.finished.set(s.finished.get() + 1);
        s.trace
            .borrow_mut()
            .closes
            .push(json!({"guest":self.index,"code":1000,"reason":"Public smoke complete"}));
        s.trace
            .borrow_mut()
            .order
            .push(format!("close:{}", self.index));
        if Instant::now() >= deadline {
            return Err(fail(124, "cleanup_deadline"));
        }
        if s.yes("lateClose") || s.yes("closeAtDeadline") {
            let start = s.cleanup_anchor.get().unwrap_or_else(|| {
                s.cleanup_anchor.set(Some(s.clock.get()));
                s.clock.get()
            });
            s.clock.set(start + 5.0);
        }
        if s.yes("closeMissing") || s.yes("closeThrow") {
            let start = s.cleanup_anchor.get().unwrap_or_else(|| {
                s.cleanup_anchor.set(Some(s.clock.get()));
                s.clock.get()
            });
            s.clock.set(start + 5.0);
            return Err(fail(124, "cleanup_deadline"));
        }
        Ok(CloseObservation {
            clean: !s.yes("closeUnclean"),
        })
    }
}
