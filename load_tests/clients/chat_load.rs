// Opt-in text traffic sharing the media clients' real signaling connections.
// One lock per room, no work on the RTP path, and a bounded delivery inventory.
use anyhow::{Context, Result};
use serde::Serialize;
use simplestChat::signaling::protocol::{ClientMessage, ServerMessage};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const DRAIN: Duration = Duration::from_secs(2);
const MAX_DELIVERIES: usize = 1_000_000;
const CONTENT_BYTES: usize = 128;

#[derive(Debug)]
pub struct ChatLoad {
    start: Instant,
    stop: Instant,
    interval: Duration,
    clients: usize,
    rooms: Vec<Mutex<Room>>,
    prefix: String,
    invalid_observations: AtomicUsize,
}

#[derive(Debug, Default)]
struct Room {
    participants: HashMap<usize, String>,
    messages: HashMap<String, Sent>,
}

#[derive(Debug)]
struct Sent {
    sender: usize,
    content: String,
    at: Instant,
    message_id: Option<String>,
    acknowledged: bool,
    received: HashSet<usize>,
}

#[derive(Debug, Clone)]
pub struct ChatClient {
    load: Arc<ChatLoad>,
    index: usize,
}

#[derive(Debug)]
pub struct ChatSchedule {
    client: ChatClient,
    next: Instant,
    sequence: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatReport {
    pub passed: bool,
    pub interval_ms: u64,
    pub content_bytes: usize,
    pub drain_ms: u64,
    pub clients_registered: usize,
    pub expected_messages: usize,
    pub sent_messages: usize,
    pub acknowledgements: usize,
    pub expected_deliveries: usize,
    pub received_deliveries: usize,
    pub invalid_observations: usize,
    pub failure_reasons: Vec<String>,
}

/// Restrict this additional workload to stable clients on an owned local server.
/// The per-room rate leaves half the application's ten-message/second budget.
pub fn validate(
    server_url: &str,
    clients: usize,
    rooms: usize,
    interval_ms: Option<u64>,
    duration_secs: u64,
    churn_rate: f64,
) -> Result<()> {
    let Some(interval_ms) = interval_ms else {
        return Ok(());
    };
    let url = url::Url::parse(server_url)?;
    anyhow::ensure!(
        matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")),
        "--chat-interval-ms requires an owned loopback server"
    );
    anyhow::ensure!(
        churn_rate == 0.0,
        "Chat delivery coverage requires --churn-rate 0"
    );
    anyhow::ensure!(
        (1_000..=600_000).contains(&interval_ms),
        "--chat-interval-ms must be 1000..600000"
    );
    anyhow::ensure!(
        clients > 0 && rooms > 0 && rooms <= clients,
        "Chat needs nonempty rooms"
    );
    let room_size = clients.div_ceil(rooms);
    anyhow::ensure!(
        room_size as u64 * 1_000 <= interval_ms * 5,
        "Chat is bounded to five messages/second per room; increase --chat-interval-ms"
    );
    let duration_ms = duration_secs.saturating_mul(1_000);
    anyhow::ensure!(
        duration_ms >= interval_ms + DRAIN.as_millis() as u64,
        "Chat duration must include one interval per client plus two seconds for delivery"
    );
    let upper_messages = clients.saturating_mul(duration_ms.div_ceil(interval_ms) as usize);
    anyhow::ensure!(
        upper_messages.saturating_mul(room_size) <= MAX_DELIVERIES,
        "Chat delivery inventory exceeds its one-million-event bound; shorten the run or slow chat"
    );
    Ok(())
}

impl ChatLoad {
    pub fn new(
        clients: usize,
        rooms: usize,
        interval_ms: u64,
        start: Instant,
        end: Instant,
    ) -> Arc<Self> {
        Arc::new(Self {
            start,
            stop: end - DRAIN,
            interval: Duration::from_millis(interval_ms),
            clients,
            rooms: (0..rooms).map(|_| Mutex::new(Room::default())).collect(),
            prefix: format!("load-chat-{}-", uuid::Uuid::new_v4().simple()),
            invalid_observations: AtomicUsize::new(0),
        })
    }

    pub fn client(self: &Arc<Self>, index: usize) -> ChatClient {
        ChatClient {
            load: Arc::clone(self),
            index,
        }
    }

    fn room_size(&self, index: usize) -> usize {
        let room = index % self.rooms.len();
        (self.clients - 1 - room) / self.rooms.len() + 1
    }

    fn first_send(&self, index: usize) -> Instant {
        let seat = index / self.rooms.len();
        self.start
            + self
                .interval
                .mul_f64(seat as f64 / self.room_size(index) as f64)
    }

    fn expected_messages(&self, index: usize) -> usize {
        let first = self.first_send(index);
        if first >= self.stop {
            return 0;
        }
        self.stop
            .duration_since(first)
            .as_nanos()
            .div_ceil(self.interval.as_nanos()) as usize
    }

    pub fn report(&self) -> ChatReport {
        let mut report = ChatReport {
            passed: false,
            interval_ms: self.interval.as_millis() as u64,
            content_bytes: CONTENT_BYTES,
            drain_ms: DRAIN.as_millis() as u64,
            clients_registered: 0,
            expected_messages: (0..self.clients).map(|i| self.expected_messages(i)).sum(),
            sent_messages: 0,
            acknowledgements: 0,
            expected_deliveries: (0..self.clients)
                .map(|i| self.expected_messages(i) * (self.room_size(i) - 1))
                .sum(),
            received_deliveries: 0,
            invalid_observations: self.invalid_observations.load(Ordering::Relaxed),
            failure_reasons: Vec::new(),
        };
        for room in &self.rooms {
            let room = room.lock().unwrap();
            report.clients_registered += room.participants.len();
            report.sent_messages += room.messages.len();
            report.acknowledgements += room.messages.values().filter(|m| m.acknowledged).count();
            report.received_deliveries += room
                .messages
                .values()
                .map(|m| m.received.len())
                .sum::<usize>();
        }
        for (name, actual, expected) in [
            (
                "registered clients",
                report.clients_registered,
                self.clients,
            ),
            (
                "sent messages",
                report.sent_messages,
                report.expected_messages,
            ),
            (
                "acknowledgements",
                report.acknowledgements,
                report.expected_messages,
            ),
            (
                "peer deliveries",
                report.received_deliveries,
                report.expected_deliveries,
            ),
            ("invalid observations", report.invalid_observations, 0),
        ] {
            if actual != expected {
                report.failure_reasons.push(format!(
                    "Chat {name}: expected {expected}, observed {actual}"
                ));
            }
        }
        report.passed = report.failure_reasons.is_empty();
        report
    }
}

impl ChatClient {
    pub fn register(&self, participant_id: &str) -> Result<()> {
        let mut room = self.load.rooms[self.index % self.load.rooms.len()]
            .lock()
            .unwrap();
        anyhow::ensure!(
            !room.participants.contains_key(&self.index),
            "Chat participant registered more than once"
        );
        room.participants
            .insert(self.index, participant_id.to_owned());
        Ok(())
    }

    pub fn schedule(self) -> ChatSchedule {
        ChatSchedule {
            next: self.load.first_send(self.index),
            client: self,
            sequence: 0,
        }
    }

    /// Observe validated ACKs and each peer's delivery, in either arrival order.
    /// Successful observations also feed the existing exact latency histograms.
    pub fn observe(
        &self,
        message: &ServerMessage,
        now: Instant,
    ) -> Result<Option<(&'static str, u64)>> {
        let result = self.observe_inner(message, now);
        if result.is_err() {
            self.load
                .invalid_observations
                .fetch_add(1, Ordering::Relaxed);
        }
        result
    }

    fn observe_inner(
        &self,
        message: &ServerMessage,
        now: Instant,
    ) -> Result<Option<(&'static str, u64)>> {
        let (id, participant, content, server_id, acknowledgement) = match message {
            ServerMessage::MessageAck {
                client_message_id,
                message,
            } => {
                anyhow::ensure!(
                    client_message_id == &message.client_message_id,
                    "Chat ACK identity changed"
                );
                anyhow::ensure!(
                    message.recipient_id.is_none() && message.reply_to.is_none(),
                    "Chat ACK changed conversation"
                );
                (
                    client_message_id,
                    &message.participant_id,
                    &message.content,
                    &message.message_id,
                    true,
                )
            }
            ServerMessage::ChatReceived {
                client_message_id,
                participant_id,
                content,
                message_id,
                ..
            } => (
                client_message_id,
                participant_id,
                content,
                message_id,
                false,
            ),
            _ => return Ok(None),
        };
        anyhow::ensure!(
            id.starts_with(&self.load.prefix),
            "Unexpected chat identity on an owned test connection"
        );
        let mut room = self.load.rooms[self.index % self.load.rooms.len()]
            .lock()
            .unwrap();
        let sender = room
            .messages
            .get(id)
            .context("Chat response has no owned send in this room")?
            .sender;
        anyhow::ensure!(
            room.participants.get(&sender) == Some(participant),
            "Chat sender identity changed"
        );
        let sent = room.messages.get_mut(id).unwrap();
        anyhow::ensure!(
            &sent.content == content && !server_id.is_empty(),
            "Chat content or server identity changed"
        );
        if let Some(expected) = &sent.message_id {
            anyhow::ensure!(
                expected == server_id,
                "Chat ACK and fanout disagree on the server message ID"
            );
        }
        let latency = now.saturating_duration_since(sent.at);
        anyhow::ensure!(
            latency <= DRAIN,
            "Chat ACK or peer delivery exceeded two seconds"
        );
        if acknowledgement {
            anyhow::ensure!(
                self.index == sender && !sent.acknowledged,
                "Duplicate or misrouted chat ACK"
            );
            sent.acknowledged = true;
        } else {
            anyhow::ensure!(
                self.index != sender && sent.received.insert(self.index),
                "Duplicate or self chat fanout"
            );
        }
        sent.message_id = Some(server_id.clone());
        Ok(Some((
            if acknowledgement {
                "chat_ack"
            } else {
                "chat_delivery"
            },
            latency.as_millis() as u64,
        )))
    }
}

impl ChatSchedule {
    pub fn next(&self) -> Option<Instant> {
        (self.next < self.client.load.stop).then_some(self.next)
    }

    /// Skip missed slots instead of catching up in a burst. The final expected
    /// count makes a starved generator fail rather than silently lower its load.
    pub fn prepare(&mut self, now: Instant) -> Result<Option<ClientMessage>> {
        if now < self.next || now >= self.client.load.stop {
            return Ok(None);
        }
        self.sequence += 1;
        let id = format!(
            "{}{}-{}",
            self.client.load.prefix, self.client.index, self.sequence
        );
        let mut content = format!(
            "Owned load-test text from participant {} message {}. ",
            self.client.index, self.sequence
        );
        content.extend(std::iter::repeat_n(
            'x',
            CONTENT_BYTES.saturating_sub(content.len()),
        ));
        let mut room = self.client.load.rooms[self.client.index % self.client.load.rooms.len()]
            .lock()
            .unwrap();
        anyhow::ensure!(
            room.participants.contains_key(&self.client.index),
            "Chat sender did not join"
        );
        anyhow::ensure!(
            self.sequence as usize <= self.client.load.expected_messages(self.client.index),
            "Chat emitted more messages than its bounded schedule"
        );
        room.messages.insert(
            id.clone(),
            Sent {
                sender: self.client.index,
                content: content.clone(),
                at: now,
                message_id: None,
                acknowledged: false,
                received: HashSet::new(),
            },
        );
        self.next = now + self.client.load.interval;
        Ok(Some(ClientMessage::ChatMessage {
            content,
            client_message_id: Some(id),
            sequence: Some(self.sequence),
            reply_to: None,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use simplestChat::signaling::protocol::{ChatEntry, ChatStyle};

    fn load(clients: usize, rooms: usize, interval: u64, duration: u64) -> Arc<ChatLoad> {
        validate(
            "ws://127.0.0.1/ws",
            clients,
            rooms,
            Some(interval),
            duration,
            0.0,
        )
        .unwrap();
        let start = Instant::now();
        let load = ChatLoad::new(
            clients,
            rooms,
            interval,
            start,
            start + Duration::from_secs(duration),
        );
        for index in 0..clients {
            load.client(index)
                .register(&format!("participant-{index}"))
                .unwrap();
        }
        load
    }

    fn responses(command: ClientMessage, index: usize) -> (ServerMessage, ServerMessage) {
        let ClientMessage::ChatMessage {
            content,
            client_message_id: Some(id),
            sequence: Some(sequence),
            ..
        } = command
        else {
            panic!("Expected an owned sequenced chat command");
        };
        assert_eq!(content.len(), CONTENT_BYTES);
        let entry = ChatEntry {
            message_id: format!("server-{index}-{sequence}"),
            client_message_id: id.clone(),
            participant_id: format!("participant-{index}"),
            participant_name: format!("TestUser{index}"),
            recipient_id: None,
            recipient_name: None,
            content,
            sent_at: "2026-09-30T00:00:00Z".into(),
            chat_style: ChatStyle::default(),
            reply_to: None,
            reactions: Vec::new(),
        };
        (
            ServerMessage::MessageAck {
                client_message_id: id,
                message: entry.clone(),
            },
            ServerMessage::ChatReceived {
                participant_id: entry.participant_id,
                participant_name: entry.participant_name,
                content: entry.content,
                message_id: entry.message_id,
                client_message_id: entry.client_message_id,
                sent_at: entry.sent_at,
                chat_style: entry.chat_style,
                reply_to: None,
            },
        )
    }

    #[test]
    fn optional_chat_is_bounded_before_any_connection() {
        assert!(validate("ws://example.invalid", 30, 1, None, 60, 1.0).is_ok());
        assert!(validate("ws://127.0.0.1/ws", 30, 1, Some(30_000), 60, 0.0).is_ok());
        for (url, clients, rooms, interval, duration, churn) in [
            ("ws://example.invalid", 30, 1, 30_000, 60, 0.0),
            ("ws://127.0.0.1", 30, 1, 999, 60, 0.0),
            ("ws://127.0.0.1", 30, 1, 600_001, 3600, 0.0),
            ("ws://127.0.0.1", 30, 0, 30_000, 60, 0.0),
            ("ws://127.0.0.1", 30, 31, 30_000, 60, 0.0),
            ("ws://127.0.0.1", 30, 1, 1_000, 60, 0.0),
            ("ws://127.0.0.1", 30, 1, 30_000, 31, 0.0),
            ("ws://127.0.0.1", 30, 1, 30_000, 60, 1.0),
            ("ws://127.0.0.1", 10_000, 334, 30_000, 3600, 0.0),
        ] {
            assert!(validate(url, clients, rooms, Some(interval), duration, churn).is_err());
        }
    }

    #[test]
    fn thirty_members_have_one_second_phases_and_two_second_drain() {
        let load = load(60, 2, 30_000, 60);
        for index in 0..60 {
            let schedule = load.client(index).schedule();
            assert_eq!(
                schedule.next().unwrap() - load.start,
                Duration::from_secs((index / 2) as u64)
            );
        }
        let report = load.report();
        assert_eq!(report.expected_messages, 116);
        assert_eq!(report.expected_deliveries, 116 * 29);
        assert!(!report.passed);
    }

    #[test]
    fn every_peer_and_ack_must_match_in_either_arrival_order() {
        for ack_first in [false, true] {
            let load = load(5, 2, 3_000, 5);
            for index in 0..5 {
                let client = load.client(index);
                let mut schedule = client.clone().schedule();
                let now = schedule.next().unwrap();
                let (ack, event) = responses(schedule.prepare(now).unwrap().unwrap(), index);
                if ack_first {
                    client.observe(&ack, now).unwrap();
                }
                for peer in (index % 2..5).step_by(2).filter(|peer| *peer != index) {
                    assert_eq!(
                        load.client(peer)
                            .observe(&event, now + Duration::from_millis(7))
                            .unwrap(),
                        Some(("chat_delivery", 7))
                    );
                }
                if !ack_first {
                    client.observe(&ack, now).unwrap();
                }
                assert!(schedule.next().is_none());
            }
            let report = load.report();
            assert!(report.passed, "{:?}", report.failure_reasons);
            assert_eq!(
                (
                    report.sent_messages,
                    report.acknowledgements,
                    report.received_deliveries
                ),
                (5, 5, 8)
            );
        }
    }

    #[test]
    fn missing_fanout_never_passes_even_with_successful_acks() {
        let load = load(2, 1, 1_000, 3);
        for index in 0..2 {
            let client = load.client(index);
            let mut schedule = client.clone().schedule();
            let now = schedule.next().unwrap();
            let (ack, _) = responses(schedule.prepare(now).unwrap().unwrap(), index);
            client.observe(&ack, now).unwrap();
        }
        let report = load.report();
        assert_eq!(report.acknowledgements, 2);
        assert!(!report.passed);
        assert!(
            report
                .failure_reasons
                .iter()
                .any(|reason| reason.contains("peer deliveries"))
        );
    }

    #[test]
    fn duplicate_misrouted_changed_and_late_messages_fail() {
        let load = load(3, 1, 3_000, 5);
        let sender = load.client(0);
        let mut schedule = sender.clone().schedule();
        let now = schedule.next().unwrap();
        let (ack, event) = responses(schedule.prepare(now).unwrap().unwrap(), 0);
        assert!(load.client(1).observe(&ack, now).is_err());
        assert!(sender.observe(&event, now).is_err());
        sender.observe(&ack, now).unwrap();
        assert!(sender.observe(&ack, now).is_err());
        load.client(1).observe(&event, now).unwrap();
        assert!(load.client(1).observe(&event, now).is_err());
        let mut changed = event.clone();
        if let ServerMessage::ChatReceived { message_id, .. } = &mut changed {
            *message_id = "different".into();
        }
        assert!(load.client(2).observe(&changed, now).is_err());
        assert!(
            load.client(2)
                .observe(&event, now + Duration::from_secs(3))
                .is_err()
        );
        assert_eq!(load.report().invalid_observations, 6);
        assert!(!load.report().passed);
    }

    #[test]
    fn missed_schedule_never_bursts_or_disappears_from_the_gate() {
        let load = load(1, 1, 1_000, 5);
        let mut schedule = load.client(0).schedule();
        let late = load.start + Duration::from_secs(2);
        assert!(schedule.prepare(late).unwrap().is_some());
        assert!(schedule.prepare(late).unwrap().is_none());
        assert!(schedule.next().is_none());
        let report = load.report();
        assert_eq!((report.expected_messages, report.sent_messages), (3, 1));
        assert!(!report.passed);
    }
}
