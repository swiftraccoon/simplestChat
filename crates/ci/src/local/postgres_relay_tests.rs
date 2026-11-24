//! The relay against what the retired Python relay answered: its gateway
//! rule on recorded addresses and its behaviour in recorded scenarios over
//! real loopback sockets, each case bound by the SHA-256 of its canonical
//! JSON description.

use super::*;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Instant;
use tokio::task::JoinHandle;

const RECORDED: &str = include_str!("recorded_postgres_relay.json");

fn recorded(section: &str) -> Vec<Value> {
    let value: Value = serde_json::from_str(RECORDED).unwrap();
    let cases = value[section].as_array().unwrap().clone();
    for case in &cases {
        let description = serde_json::to_vec(&case["description"]).unwrap();
        assert_eq!(
            case["sha256"],
            crate::io::sha256(&description),
            "{}",
            case["description"]
        );
    }
    cases
}

#[test]
fn every_recorded_gateway_is_judged_as_the_reference_judged_it() {
    let cases = recorded("gateways");
    assert_eq!(cases.len(), 26);
    let mut listening = 0;
    for case in cases {
        let address: Ipv4Addr = case["description"]["address"]
            .as_str()
            .unwrap()
            .parse()
            .unwrap();
        let judged = gateway([SocketAddr::from((address, 5432))]);
        if case["ending"] == "listening" {
            listening += 1;
            assert_eq!(judged, Ok(address), "{address}");
        } else {
            assert_eq!(
                judged.map_err(Refusal::words),
                Err(case["words"].as_str().unwrap())
            );
        }
    }
    assert_eq!(listening, 17);
}

#[test]
fn the_gateway_is_the_first_ipv4_address_and_none_is_no_gateway() {
    let v6 = SocketAddr::from((std::net::Ipv6Addr::LOCALHOST, 5432));
    let first = SocketAddr::from(([10, 88, 0, 1], 5432));
    let second = SocketAddr::from(([8, 8, 8, 8], 5432));
    assert_eq!(
        gateway([v6, first, second]),
        Ok(Ipv4Addr::new(10, 88, 0, 1))
    );
    assert_eq!(gateway([v6, second, first]), Err(Refusal::Public));
    assert_eq!(gateway([v6]), Err(Refusal::Unresolved));
    assert_eq!(gateway([]), Err(Refusal::Unresolved));
}

#[test]
fn only_a_decimal_port_from_one_through_65535_is_accepted() {
    for (text, port) in [
        ("5432", Some(5432)),
        ("1", Some(1)),
        ("65535", Some(65535)),
        ("05435", Some(5435)),
        ("0", None),
        ("65536", None),
        ("+5432", None),
        (" 5432", None),
        ("5432 ", None),
        ("", None),
        ("54x2", None),
    ] {
        assert_eq!(parse_port(text), port, "{text:?}");
    }
}

#[test]
fn other_arguments_are_a_usage_error_before_any_lookup() {
    for arguments in [
        &[][..],
        &["5432"],
        &["5432", "ready", "extra"],
        &["0", "ready"],
    ] {
        let arguments: Vec<String> = arguments.iter().map(|text| (*text).to_owned()).collect();
        assert_eq!(run(&arguments).unwrap(), ExitCode::from(2));
    }
}

#[test]
fn readiness_is_recorded_once_in_a_new_file() {
    let path = std::env::temp_dir().join(format!("postgres-relay-ready-{}", std::process::id()));
    record_ready(&path).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"ready\n");
    let again = record_ready(&path).unwrap_err();
    std::fs::remove_file(&path).unwrap();
    assert_eq!(again.kind(), io::ErrorKind::AlreadyExists);
}

#[tokio::test]
async fn a_port_another_listener_holds_is_refused() {
    let held = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = held.local_addr().unwrap().port();
    assert!(listen(port).is_err());
    drop(held);
}

#[tokio::test]
async fn a_service_that_never_answers_closes_the_client_at_the_connect_deadline() {
    let listener = listen(0).unwrap();
    let address = listener.local_addr().unwrap();
    let deadlines = Deadlines {
        connect: Duration::from_millis(300),
        idle: Duration::from_secs(5),
    };
    let accepted = tokio::spawn(async move {
        let (client, _) = listener.accept().await.unwrap();
        relay(client, std::future::pending(), deadlines).await;
    });
    let started = Instant::now();
    let mut client = TcpStream::connect(address).await.unwrap();
    let mut buffer = [0; 16];
    let read = tokio::time::timeout(Duration::from_secs(5), client.read(&mut buffer)).await;
    assert_eq!(read.unwrap().unwrap(), 0);
    let waited = started.elapsed();
    assert!(waited >= Duration::from_millis(250) && waited < Duration::from_secs(4));
    accepted.await.unwrap();
}

/// The bytes a bulk step sends: 0 through 250, repeated.
fn pattern(size: usize) -> Vec<u8> {
    (0..size).map(|at| (at % 251) as u8).collect()
}

/// One side of a relayed connection, split so a background writer can own
/// the sending half while the scenario reads.
#[derive(Default)]
struct Side {
    read: Option<OwnedReadHalf>,
    write: Option<OwnedWriteHalf>,
}

/// Runs a recorded scenario's steps, observing what each side sees, as the
/// recorder's driver did.
struct Driver {
    relay: SocketAddr,
    service: Option<TcpListener>,
    client: Side,
    upstream: Side,
    mark: Instant,
    background: Vec<JoinHandle<()>>,
}

impl Driver {
    fn side(&mut self, name: &Value) -> &mut Side {
        match name.as_str().unwrap() {
            "client" => &mut self.client,
            "upstream" => &mut self.upstream,
            other => panic!("unknown side {other}"),
        }
    }

    /// Read up to `size` bytes, or what arrives before an end or `limit`.
    async fn receive(&mut self, name: &Value, size: usize, limit: Duration) -> Vec<u8> {
        let reader = self.side(name).read.as_mut().unwrap();
        let mut data = vec![0; size];
        let mut filled = 0;
        let ends = Instant::now() + limit;
        while filled < size {
            let left = ends.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, reader.read(&mut data[filled..])).await {
                Ok(Ok(read)) if read > 0 => filled += read,
                _ => break,
            }
        }
        data.truncate(filled);
        data
    }

    /// What arrives until the end of the stream or `limit`, and whether the
    /// end (or a reset) came.
    async fn until_end(&mut self, name: &Value, limit: Duration) -> (Vec<u8>, bool) {
        let reader = self.side(name).read.as_mut().unwrap();
        let mut data = Vec::new();
        let mut buffer = vec![0; CHUNK];
        let ends = Instant::now() + limit;
        loop {
            let left = ends.saturating_duration_since(Instant::now());
            match tokio::time::timeout(left, reader.read(&mut buffer)).await {
                Ok(Ok(0)) => return (data, true),
                Ok(Ok(read)) => data.extend_from_slice(&buffer[..read]),
                Ok(Err(error)) => return (data, error.kind() == io::ErrorKind::ConnectionReset),
                Err(_) => return (data, false),
            }
        }
    }

    async fn step(&mut self, step: &[Value]) -> Value {
        let text = |at: usize| step[at].as_str().unwrap().to_owned();
        let number = |at: usize| usize::try_from(step[at].as_u64().unwrap()).unwrap();
        match step[0].as_str().unwrap() {
            "connect" => {
                let (read, write) = TcpStream::connect(self.relay).await.unwrap().into_split();
                self.client = Side {
                    read: Some(read),
                    write: Some(write),
                };
                Value::Null
            }
            "accept" => {
                let service = self.service.as_ref().unwrap();
                match tokio::time::timeout(Duration::from_secs(3), service.accept()).await {
                    Ok(Ok((stream, _))) => {
                        let (read, write) = stream.into_split();
                        self.upstream = Side {
                            read: Some(read),
                            write: Some(write),
                        };
                        json!("accepted")
                    }
                    _ => json!("none"),
                }
            }
            "send" => {
                let writer = self.side(&step[1]).write.as_mut().unwrap();
                writer.write_all(text(2).as_bytes()).await.unwrap();
                Value::Null
            }
            "shutdown" => {
                self.side(&step[1])
                    .write
                    .as_mut()
                    .unwrap()
                    .shutdown()
                    .await
                    .unwrap();
                Value::Null
            }
            "close" => {
                *self.side(&step[1]) = Side::default();
                Value::Null
            }
            "receive" => {
                let data = self
                    .receive(&step[1], number(2), Duration::from_secs(3))
                    .await;
                json!(String::from_utf8(data).unwrap())
            }
            "end" => {
                let (data, ended) = self.until_end(&step[1], Duration::from_secs(5)).await;
                json!({"text": String::from_utf8(data).unwrap(), "ended": ended})
            }
            "end_short" => {
                let (data, ended) = self.until_end(&step[1], Duration::from_secs(5)).await;
                json!({"ended": ended, "short": data.len() < number(2)})
            }
            "end_digest" => {
                let (data, ended) = self.until_end(&step[1], Duration::from_secs(10)).await;
                let digest = crate::io::hex(&Sha256::digest(&data));
                json!({"bytes": data.len(), "sha256": digest, "ended": ended})
            }
            "stream" => {
                let mut writer = self.side(&step[1]).write.take().unwrap();
                let (count, interval, chunk) = (number(2), number(3), text(4));
                self.background.push(tokio::spawn(async move {
                    for _ in 0..count {
                        if writer.write_all(chunk.as_bytes()).await.is_err() {
                            return;
                        }
                        tokio::time::sleep(Duration::from_millis(interval as u64)).await;
                    }
                }));
                Value::Null
            }
            "bulk" => {
                let mut writer = self.side(&step[1]).write.take().unwrap();
                let data = pattern(number(2));
                self.background.push(tokio::spawn(async move {
                    if writer.write_all(&data).await.is_ok() {
                        writer.shutdown().await.ok();
                    }
                }));
                Value::Null
            }
            "mark" => {
                self.mark = Instant::now();
                Value::Null
            }
            "since_mark_at_least" => {
                let seconds = Duration::from_secs_f64(step[1].as_f64().unwrap());
                json!(self.mark.elapsed() >= seconds)
            }
            other => panic!("unknown step {other}"),
        }
    }
}

#[tokio::test]
async fn every_recorded_scenario_is_relayed_as_the_reference_relayed_it() {
    let cases = recorded("scenarios");
    assert_eq!(cases.len(), 8);
    for case in cases {
        let description = &case["description"];
        let service = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let upstream = match service.local_addr().unwrap() {
            SocketAddr::V4(address) => address,
            SocketAddr::V6(_) => unreachable!("bound to an IPv4 address"),
        };
        let service = if description["upstream"] == "listening" {
            service.set_nonblocking(true).unwrap();
            Some(TcpListener::from_std(service).unwrap())
        } else {
            drop(service);
            None
        };
        let listener = listen(0).unwrap();
        let relay = listener.local_addr().unwrap();
        let deadlines = Deadlines {
            connect: Duration::from_secs(2),
            idle: Duration::from_secs_f64(description["idle"].as_f64().unwrap()),
        };
        let server = tokio::spawn(serve(listener, upstream, deadlines));
        let mut driver = Driver {
            relay,
            service,
            client: Side::default(),
            upstream: Side::default(),
            mark: Instant::now(),
            background: Vec::new(),
        };
        let mut observed = Vec::new();
        for step in description["steps"].as_array().unwrap() {
            observed.push(driver.step(step.as_array().unwrap()).await);
        }
        for task in driver.background.drain(..) {
            tokio::time::timeout(Duration::from_secs(10), task)
                .await
                .unwrap()
                .unwrap();
        }
        server.abort();
        assert_eq!(
            Value::Array(observed),
            case["observed"],
            "{}",
            description["name"]
        );
    }
}
