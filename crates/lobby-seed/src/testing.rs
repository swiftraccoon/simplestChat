//! The recorded runs of the earlier seeding helper, and a scripted loopback
//! backend that gives each connection the next recorded answer and keeps
//! what was asked of it.

use serde_json::Value;
use std::collections::BTreeMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::Duration;

/// Every recorded scenario, with its tables of trees, requests, answers and
/// reports, and the vectors of the pure admission rules.
pub const RECORDED: &str = include_str!("../fixtures/recorded_seed.json");

/// The recording's placeholders with the fixture values they stand for.
pub fn concrete(text: &str) -> String {
    let fixtures = [
        (
            "<password A>",
            format!("Private-fixture-phrase-{}", "x".repeat(32)),
        ),
        (
            "<password B>",
            format!("Another-fixture-phrase-{}", "y".repeat(32)),
        ),
        (
            "<token>",
            format!("owner-access-fixture-{}", "0".repeat(22)),
        ),
        ("<refresh>", format!("refresh-fixture-{}", "r".repeat(27))),
    ];
    let mut text = text.to_owned();
    for (placeholder, value) in fixtures {
        text = text.replace(placeholder, &value);
    }
    text
}

/// A recorded run whose outcome here is deliberately another one.
pub struct Stricter {
    pub scenario: &'static str,
    pub failure: &'static str,
    /// Whether the session was ended.
    pub revoked: Option<bool>,
    /// How many requests are made.
    pub requests: usize,
    /// Whether an existing lobby was found.
    pub preserved: bool,
}

const fn tighter(
    scenario: &'static str,
    failure: &'static str,
    revoked: Option<bool>,
    requests: usize,
    preserved: bool,
) -> Stricter {
    Stricter {
        scenario,
        failure,
        revoked,
        requests,
        preserved,
    }
}

const IDENTITY: &str = "invalid_owner_identity";
const NOT_ENDED: &str = "refresh_session_cleanup_failed";
const UNREADABLE: &str = "seed_operation_failed";

/// The earlier client was lenient where this one refuses: an identity in an
/// unusual spelling, a cookie lifetime given as a date, a clearing cookie
/// without the secure mark, a head without its end, a provisional answer, a
/// folded header, lengths that are not one plain number, and a body in
/// another encoding or behind a byte order mark. Where this client stops
/// early, its sign-out meets the answer recorded for the request it never
/// made, so the session counts as not ended.
pub const STRICTER: [Stricter; 14] = [
    tighter(
        "a user identity that is without hyphens",
        IDENTITY,
        Some(false),
        2,
        false,
    ),
    tighter(
        "a user identity that is in braces",
        IDENTITY,
        Some(false),
        2,
        false,
    ),
    tighter(
        "a user identity that is a URN",
        IDENTITY,
        Some(false),
        2,
        false,
    ),
    tighter(
        "a user identity that is with other hyphens",
        IDENTITY,
        Some(false),
        2,
        false,
    ),
    tighter(
        "a cookie with a future date",
        "refresh_cookie_missing",
        Some(false),
        1,
        false,
    ),
    tighter(
        "a sign-out that clears without the secure mark",
        NOT_ENDED,
        Some(false),
        3,
        true,
    ),
    tighter(
        "a sign-out that clears by date",
        NOT_ENDED,
        Some(false),
        3,
        true,
    ),
    tighter(
        "a backend that answers with a status only",
        UNREADABLE,
        None,
        1,
        false,
    ),
    tighter(
        "a continue before the answer",
        "owner_login_failed",
        None,
        1,
        false,
    ),
    tighter("an answer with a folded header", UNREADABLE, None, 1, false),
    tighter("an answer with two lengths", UNREADABLE, None, 1, false),
    tighter(
        "an answer with a length that is not a number",
        UNREADABLE,
        None,
        1,
        false,
    ),
    tighter(
        "an answer that starts with a byte order mark",
        "invalid_json",
        Some(false),
        2,
        false,
    ),
    tighter(
        "an answer in sixteen-bit units",
        "invalid_json",
        Some(false),
        2,
        false,
    ),
];

/// The stricter outcome of a recorded scenario, if it has one.
pub fn stricter(scenario: &str) -> Option<&'static Stricter> {
    STRICTER.iter().find(|outcome| outcome.scenario == scenario)
}

/// What the backend does with one connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Send these bytes and close.
    Answer(Vec<u8>),
    /// Close without a word.
    Close,
    /// Keep the connection open in silence.
    Hang,
}

/// One request as the backend received it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Received {
    pub method: String,
    pub target: String,
    /// Header names in lower case.
    pub headers: BTreeMap<String, String>,
    pub body: String,
}

/// Recorded text whose characters are single bytes.
fn bytes(text: &str) -> Vec<u8> {
    let narrow = |character: char| u8::try_from(u32::from(character)).unwrap();
    concrete(text).chars().map(narrow).collect()
}

/// The scripted answers of a recorded scenario, in order.
pub fn actions(recorded: &Value, scenario: &Value) -> Vec<Action> {
    let exchanges = scenario["exchanges"].as_array().unwrap();
    let answer = |exchange: &Value| {
        let index = usize::try_from(exchange[1].as_u64().unwrap()).unwrap();
        let response = &recorded["responses"][index];
        if response["close"] == true {
            Action::Close
        } else if response["hang"] == true {
            Action::Hang
        } else if let Some(raw) = response["raw"].as_str() {
            Action::Answer(bytes(raw))
        } else {
            let repeat = &response["rawRepeat"];
            let count = usize::try_from(repeat["count"].as_u64().unwrap()).unwrap();
            let mut raw = bytes(repeat["prefix"].as_str().unwrap());
            raw.extend(bytes(repeat["fill"].as_str().unwrap()).repeat(count));
            raw.extend(bytes(repeat["suffix"].as_str().unwrap()));
            Action::Answer(raw)
        }
    };
    exchanges.iter().map(answer).collect()
}

/// The requests of a recorded scenario, in order, with only the headers the
/// recording kept.
pub fn requests(recorded: &Value, scenario: &Value) -> Vec<Received> {
    let exchanges = scenario["exchanges"].as_array().unwrap();
    let request = |exchange: &Value| {
        let index = usize::try_from(exchange[0].as_u64().unwrap()).unwrap();
        let request = &recorded["requests"][index];
        let headers = request["headers"].as_object().unwrap();
        let header =
            |(name, value): (&String, &Value)| (name.clone(), concrete(value.as_str().unwrap()));
        Received {
            method: request["method"].as_str().unwrap().to_owned(),
            target: request["target"].as_str().unwrap().to_owned(),
            headers: headers.iter().map(header).collect(),
            body: concrete(request["body"].as_str().unwrap()),
        }
    };
    exchanges.iter().map(request).collect()
}

/// The headers a recording keeps of a request: the others are the
/// connection's own.
pub fn kept(received: &Received) -> Received {
    let named = [
        "origin",
        "accept",
        "authorization",
        "content-type",
        "cookie",
    ];
    let mut kept = received.clone();
    kept.headers
        .retain(|name, _| named.contains(&name.as_str()));
    kept
}

/// A loopback server that plays a script, one action per connection.
pub struct Backend {
    pub address: SocketAddr,
    stop: Arc<AtomicBool>,
    worker: JoinHandle<(Vec<Received>, usize)>,
}

fn receive(connection: &mut TcpStream) -> Option<Received> {
    connection
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut data = Vec::new();
    let mut piece = [0_u8; 8192];
    let end = loop {
        if let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n") {
            break end + 4;
        }
        let count = connection.read(&mut piece).ok()?;
        if count == 0 {
            return None;
        }
        data.extend_from_slice(&piece[..count]);
    };
    let mut storage = [httparse::EMPTY_HEADER; 32];
    let mut request = httparse::Request::new(&mut storage);
    assert!(request.parse(&data[..end]).unwrap().is_complete());
    let mut headers = BTreeMap::new();
    for header in request.headers.iter() {
        let value = String::from_utf8(header.value.to_vec()).unwrap();
        let earlier = headers.insert(header.name.to_ascii_lowercase(), value);
        assert!(earlier.is_none(), "a header is sent once");
    }
    let length = headers
        .get("content-length")
        .map_or(0, |text| text.parse().unwrap());
    let method = request.method.unwrap().to_owned();
    let target = request.path.unwrap().to_owned();
    while data.len() < end + length {
        let count = connection.read(&mut piece).ok()?;
        assert!(count != 0, "a request ends after its stated length");
        data.extend_from_slice(&piece[..count]);
    }
    assert_eq!(
        data.len(),
        end + length,
        "a request ends at its stated length"
    );
    Some(Received {
        method,
        target,
        headers,
        body: String::from_utf8(data[end..].to_vec()).unwrap(),
    })
}

impl Backend {
    /// Listen on a free loopback port and play `script`.
    pub fn start(script: Vec<Action>) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = Arc::clone(&stop);
        let worker = std::thread::spawn(move || {
            let mut script = script.into_iter();
            let (mut received, mut silent) = (Vec::new(), Vec::new());
            while !stopped.load(Ordering::SeqCst) {
                let mut connection = match listener.accept() {
                    Ok((connection, _)) => connection,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    }
                    Err(error) => panic!("{error}"),
                };
                connection.set_nonblocking(false).unwrap();
                let Some(request) = receive(&mut connection) else {
                    continue;
                };
                received.push(request);
                match script.next() {
                    Some(Action::Answer(raw)) => {
                        // A client may stop reading an answer it refuses.
                        let _ = connection.write_all(&raw);
                        let _ = connection.shutdown(Shutdown::Both);
                    }
                    Some(Action::Hang) => silent.push(connection),
                    Some(Action::Close) => {}
                    None => panic!("a request beyond the script"),
                }
            }
            (received, script.count())
        });
        Self {
            address,
            stop,
            worker,
        }
    }

    /// An address on which nothing listens.
    pub fn absent() -> SocketAddr {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        listener.local_addr().unwrap()
    }

    /// Stop, and return what was received and how much of the script was
    /// never asked for.
    pub fn finish(self) -> (Vec<Received>, usize) {
        self.stop.store(true, Ordering::SeqCst);
        self.worker.join().unwrap()
    }
}
