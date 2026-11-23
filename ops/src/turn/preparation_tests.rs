//! The relay's preparation steps against the recorded answers of the
//! programs the relay playbook ran before them, and the listeners on real
//! loopback sockets.

use super::preparation::{
    LISTENERS, NOT_PUBLIC, Prepared, TAKEN, Transport, UNSAFE, bound, free, prepare, public,
};
use crate::host::fault::Fault;
use crate::test_support::{Fixture, Recording};
use serde_json::{Value, json};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;

/// The answers of the relay playbook's two inline programs at the revision
/// that replaced them (`recorded_preparation.json`).
const RECORDED: &str = include_str!("recorded_preparation.json");
const FILE_TEST: &str = "every_recorded_relay_secret_state_is_answered_as_the_program_did";
const ADDRESS_TEST: &str = "every_recorded_relay_address_is_judged_and_bound_as_the_program_did";

fn recording() -> Recording {
    Recording::new(RECORDED, &[FILE_TEST, ADDRESS_TEST])
}

/// The secret file states the program was given, in its order.
fn file_states() -> Vec<Value> {
    let valid = "0123456789abcdef".repeat(4);
    let file = |mode: &str, owner: &str, text: &str| json!({"kind": "file", "mode": mode, "owner": owner, "text": text});
    let mut cases = vec![json!({"kind": "absent"}), file("600", "root", &valid)];
    for mode in ["640", "644", "400", "700", "4600", "1600", "2600"] {
        cases.push(file(mode, "root", &valid));
    }
    cases.push(file("600", "other", &valid));
    let short = format!("{}0123456789abcde", "0123456789abcdef".repeat(3));
    for text in [
        valid.to_uppercase(),
        short.clone(),
        format!("{valid}0"),
        format!("{short}\n"),
        format!("{short}\r"),
        "0123456789abcdeg".repeat(4),
        String::new(),
        format!(" {short}"),
    ] {
        cases.push(file("600", "root", &text));
    }
    let undecodable = [b"\xff".as_slice(), &[b'0'; 63]].concat();
    let hex: String = undecodable
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    cases.push(json!({"kind": "file", "mode": "600", "owner": "root", "hex": hex}));
    let accented = format!("{}0123456789abcd\u{e9}", "0123456789abcdef".repeat(3));
    cases.push(file("600", "root", &accented));
    cases.push(json!({"kind": "link", "mode": "600", "owner": "root", "text": valid}));
    cases.push(json!({"kind": "dangling"}));
    cases.push(json!({"kind": "directory", "mode": "700", "owner": "root"}));
    cases.push(json!({"kind": "fifo", "mode": "600", "owner": "root"}));
    cases
}

/// Give `path` exactly the octal `mode`, refusing a state the system did not
/// build.
fn set_mode(path: &Path, mode: &Value) {
    let bits = u32::from_str_radix(mode.as_str().unwrap(), 8).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).unwrap();
    let built = std::fs::symlink_metadata(path).unwrap().mode() & 0o7777;
    assert_eq!(built, bits, "the fixture's mode was not kept");
}

/// The bytes a file state holds.
fn bytes(case: &Value) -> Vec<u8> {
    match case.get("hex") {
        Some(hex) => {
            let hex = hex.as_str().unwrap().as_bytes();
            let digit = |pair: &[u8]| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16);
            hex.chunks(2).map(|pair| digit(pair).unwrap()).collect()
        }
        None => case["text"]
            .as_str()
            .unwrap_or_default()
            .as_bytes()
            .to_vec(),
    }
}

/// Build one recorded state at `path`.
fn build(case: &Value, directory: &Path, path: &Path) {
    match case["kind"].as_str().unwrap() {
        "file" => {
            std::fs::write(path, bytes(case)).unwrap();
            set_mode(path, &case["mode"]);
        }
        "link" => {
            let target = directory.join("target");
            std::fs::write(&target, bytes(case)).unwrap();
            set_mode(&target, &case["mode"]);
            std::os::unix::fs::symlink(&target, path).unwrap();
        }
        "dangling" => std::os::unix::fs::symlink(directory.join("missing"), path).unwrap(),
        "directory" => {
            std::fs::create_dir(path).unwrap();
            set_mode(path, &case["mode"]);
        }
        "fifo" => {
            let mode = nix::sys::stat::Mode::from_bits_truncate(0o600);
            nix::unistd::mkfifo(path, mode).unwrap();
            set_mode(path, &case["mode"]);
        }
        kind => assert_eq!(kind, "absent"),
    }
}

/// What the program said, as the outcome the replacement must give: a
/// creation, a kept secret, the fixed refusal, or (for a traceback) any
/// refusal.
fn expected(answer: &Value) -> Option<Result<Prepared, Option<&str>>> {
    match (answer["exit"].as_i64(), answer["stdout"].as_str()) {
        (Some(0), Some("created\n")) => Some(Ok(Prepared::Created)),
        (Some(0), Some("retained\n")) => Some(Ok(Prepared::Retained)),
        (Some(1), Some("")) if answer.get("traceback").is_some() => Some(Err(None)),
        (Some(1), Some("")) => Some(Err(answer["stderr"].as_str())),
        _ => None,
    }
}

/// Every file state the program was given is answered as it answered it:
/// an absent secret is created once, private and 64 lowercase hexadecimal
/// digits, and kept unchanged the next time; a private secret of the
/// required owner is kept; every other state is refused with the
/// program's own words and left as it was. The required owner stands for
/// root.
#[test]
fn every_recorded_relay_secret_state_is_answered_as_the_program_did() {
    let mut recorded = recording().cases(FILE_TEST);
    let uid = rustix::process::geteuid().as_raw();
    let (mut created, mut retained, mut refused) = (0, 0, 0);
    for case in file_states() {
        let answer = recorded.answer(&serde_json::to_vec(&case).unwrap());
        let fixture = Fixture::new();
        let path = fixture.0.join("relay");
        build(&case, &fixture.0, &path);
        let owner = if case["owner"] == "other" {
            uid + 1
        } else {
            uid
        };
        // A pipe is never read: with no writer it would block.
        let readable = ["file", "link"].contains(&case["kind"].as_str().unwrap());
        let before = readable.then(|| std::fs::read(&path).unwrap());
        let outcome = prepare(&path, owner);
        if let Ok(done) = &outcome {
            // The word the playbook reads is the one the program printed.
            assert_eq!(answer["stdout"], format!("{}\n", done.word()), "{case}");
        }
        match expected(&answer).expect("a recorded answer of a known shape") {
            Ok(Prepared::Created) => {
                assert_eq!(outcome, Ok(Prepared::Created), "{case}");
                let metadata = std::fs::symlink_metadata(&path).unwrap();
                assert!(metadata.is_file() && metadata.mode() & 0o7777 == 0o600);
                let written = std::fs::read(&path).unwrap();
                let digits = |byte: &u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte);
                assert!(written.len() == 64 && written.iter().all(digits));
                assert_eq!(answer["again"]["stdout"], "retained\n");
                assert_eq!(prepare(&path, owner), Ok(Prepared::Retained));
                assert!(std::fs::read(&path).unwrap() == written);
                created += 1;
            }
            Ok(kept) => {
                assert_eq!(outcome, Ok(kept), "{case}");
                retained += 1;
            }
            Err(words) => {
                assert!(outcome.is_err(), "{case}");
                if let Some(words) = words {
                    assert_eq!(words, format!("{UNSAFE}\n"));
                    assert_eq!(outcome, Err(Fault::Refused(UNSAFE.into())), "{case}");
                }
                refused += 1;
            }
        }
        if let Some(before) = before {
            assert_eq!(answer["unchanged"], true);
            assert!(std::fs::read(&path).unwrap() == before, "{case}");
        }
    }
    assert_eq!(recorded.finish(), 24);
    assert_eq!((created, retained, refused), (1, 1, 22));
}

/// Each new secret is drawn afresh: two made in empty places differ.
#[test]
fn each_new_relay_secret_is_drawn_afresh() {
    let fixture = Fixture::new();
    let uid = rustix::process::geteuid().as_raw();
    let (first, second) = (fixture.0.join("first"), fixture.0.join("second"));
    assert_eq!(prepare(&first, uid), Ok(Prepared::Created));
    assert_eq!(prepare(&second, uid), Ok(Prepared::Created));
    assert!(std::fs::read(&first).unwrap() != std::fs::read(&second).unwrap());
}

/// The addresses and taken listeners the program was given, in its order.
fn address_cases() -> Vec<Value> {
    let addresses = [
        "8.8.8.8",
        "1.1.1.1",
        "203.0.113.5",
        "198.51.100.7",
        "192.0.2.10",
        "100.64.0.1",
        "100.127.255.255",
        "100.63.255.255",
        "100.128.0.0",
        "192.0.0.8",
        "192.0.0.9",
        "192.0.0.10",
        "192.0.0.11",
        "192.0.0.170",
        "192.0.0.171",
        "192.0.0.172",
        "192.0.1.1",
        "224.0.0.1",
        "239.255.255.255",
        "240.0.0.1",
        "255.255.255.254",
        "255.255.255.255",
        "0.0.0.0",
        "0.255.255.255",
        "1.0.0.0",
        "127.0.0.1",
        "126.255.255.255",
        "128.0.0.0",
        "10.0.0.1",
        "9.255.255.255",
        "11.0.0.0",
        "172.16.0.1",
        "172.31.255.255",
        "172.15.255.255",
        "172.32.0.0",
        "169.254.1.1",
        "169.253.255.255",
        "169.255.0.0",
        "192.168.1.1",
        "192.167.255.255",
        "192.169.0.0",
        "198.18.0.1",
        "198.19.255.255",
        "198.17.255.255",
        "198.20.0.0",
        "198.51.99.255",
        "198.51.101.0",
        "203.0.112.255",
        "203.0.114.0",
        "192.0.1.255",
        "192.0.3.0",
        "",
        "1.2.3",
        "1.2.3.4.5",
        "01.2.3.4",
        "1.2.3.256",
        " 8.8.8.8",
        "8.8.8.8 ",
        "8.8.8.8\n",
        "2001:db8::1",
        "::ffff:8.8.8.8",
        "\u{ff18}.8.8.8",
        "+8.8.8.8",
        "8.8.8.-8",
        "0x8.8.8.8",
        "8.8.8",
        "8..8.8",
        "1.2.3.4/32",
        "08.8.8.8",
        "8.8.8.08",
        "255.255.255.256",
        "1e1.8.8.8",
    ];
    let mut cases: Vec<Value> = addresses
        .iter()
        .map(|address| json!({"address": address, "taken": -1}))
        .collect();
    for taken in 0..3 {
        cases.push(json!({"address": "8.8.8.8", "taken": taken}));
    }
    cases
}

/// The binds the program attempted, as `[transport, address, port]`.
fn recorded_binds(answer: &Value) -> Vec<Value> {
    let events = answer["sockets"].as_array().unwrap();
    let binds = events.iter().filter(|event| event[0] == "bind");
    binds
        .map(|event| json!([event[1], event[2], event[3]]))
        .collect()
}

/// Every address the program was given is judged as it judged it, and an
/// address it accepted has the same listeners opened in the same order,
/// with the first one it found taken refusing and nothing tried after it.
/// A spelling the program's parser threw on is refused with the fixed
/// words. The binder here records and opens nothing.
#[test]
fn every_recorded_relay_address_is_judged_and_bound_as_the_program_did() {
    let mut recorded = recording().cases(ADDRESS_TEST);
    let (mut accepted, mut taken_refused, mut not_public) = (0, 0, 0);
    for case in address_cases() {
        let answer = recorded.answer(&serde_json::to_vec(&case).unwrap());
        let taken = case["taken"].as_i64().unwrap();
        let mut binds = Vec::new();
        let mut open = |transport: Transport, address: SocketAddrV4| {
            let name = match transport {
                Transport::Udp => "udp",
                Transport::Tcp => "tcp",
            };
            binds.push(json!([name, address.ip().to_string(), address.port()]));
            if i64::try_from(binds.len()).unwrap() - 1 == taken {
                return Err(std::io::ErrorKind::AddrInUse.into());
            }
            Ok(std::fs::File::open("/dev/null")?.into())
        };
        let text = case["address"].as_str().unwrap();
        let outcome = public(text).and_then(|address| free(address, &LISTENERS, &mut open));
        assert_eq!(binds, recorded_binds(&answer), "{case}");
        if answer["exit"] == 0 {
            assert_eq!(outcome, Ok(()), "{case}");
            accepted += 1;
        } else if answer["stderr"] == format!("{NOT_PUBLIC}\n") {
            assert_eq!(outcome, Err(Fault::Refused(NOT_PUBLIC.into())), "{case}");
            not_public += 1;
        } else {
            assert_eq!(answer["exit"], 1);
            let thrown = answer["traceback"].as_str().unwrap();
            let refusal = if thrown == "OSError" {
                taken_refused += 1;
                TAKEN
            } else {
                assert_eq!(thrown, "ipaddress.AddressValueError");
                not_public += 1;
                NOT_PUBLIC
            };
            assert_eq!(outcome, Err(Fault::Refused(refusal.into())), "{case}");
        }
    }
    assert_eq!(recorded.finish(), 75);
    assert_eq!((accepted, taken_refused, not_public), (28, 3, 44));
}

/// How many times the listener sequence runs on new ports before a refusal
/// of a step that must pass fails the test. Any other process on the
/// machine can hold a loopback port, for either transport, at any moment:
/// a connection's ephemeral port, or its `TIME_WAIT` after it closed, which
/// refuses a bind without address reuse. A defect refuses every attempt.
const ATTEMPTS: usize = 5;

/// Two distinct loopback ports that both transports could bind just now
/// without address reuse, as the check binds, chosen by the system for UDP
/// sockets that are held until both are found and then closed. The search
/// is bounded.
fn spare_ports() -> (u16, u16) {
    let loopback = Ipv4Addr::LOCALHOST;
    let (mut chosen, mut ports) = (Vec::new(), Vec::new());
    for _ in 0..100 {
        let socket = std::net::UdpSocket::bind(SocketAddrV4::new(loopback, 0)).unwrap();
        let port = socket.local_addr().unwrap().port();
        chosen.push(socket);
        if bound(Transport::Tcp, SocketAddrV4::new(loopback, port)).is_ok() {
            ports.push(port);
        }
        if let [udp, tcp] = ports[..] {
            return (udp, tcp);
        }
    }
    panic!("no two loopback ports were free for both transports in 100 choices");
}

/// The step `step` refused: what a direct `bound` call answers for each of
/// its listeners on the loopback address, at the moment of the refusal.
fn diagnosis(step: &str, listeners: &[(Transport, u16)]) -> String {
    let answer = |&(transport, port): &(Transport, u16)| {
        let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
        let kind = bound(transport, address)
            .map(drop)
            .map_err(|error| error.kind());
        format!("{transport:?} {address} {kind:?}")
    };
    let answers: Vec<String> = listeners.iter().map(answer).collect();
    format!("{step}: {}", answers.join(", "))
}

/// One run of the listener sequence on the spare ports `udp` and `tcp`. A
/// step that must be refused is asserted at once, since no other process
/// can make a bind succeed; a step that must pass and is refused, or a
/// socket this test cannot hold, ends the run with its diagnosis.
fn listener_sequence(udp: u16, tcp: u16) -> Result<(), String> {
    let loopback = Ipv4Addr::LOCALHOST;
    let listeners = [(Transport::Udp, udp), (Transport::Tcp, tcp)];
    let passes = |step: &str, listeners: &[(Transport, u16)]| {
        free(loopback, listeners, &mut bound).map_err(|_| diagnosis(step, listeners))
    };
    let held = |step: &str, kind: std::io::ErrorKind| format!("{step}: {kind:?}");
    passes("opened while free", &listeners)?;
    passes("opened again once closed", &listeners)?;
    let taken = Err(Fault::Refused(TAKEN.into()));
    let tcp_held = std::net::TcpListener::bind(SocketAddrV4::new(loopback, tcp))
        .map_err(|error| held("holding the Tcp port", error.kind()))?;
    assert_eq!(free(loopback, &listeners, &mut bound), taken);
    passes("Udp on the held Tcp port", &[(Transport::Udp, tcp)])?;
    drop(tcp_held);
    let udp_held = std::net::UdpSocket::bind(SocketAddrV4::new(loopback, udp))
        .map_err(|error| held("holding the Udp port", error.kind()))?;
    assert_eq!(free(loopback, &listeners, &mut bound), taken);
    passes("Tcp on the held Udp port", &[(Transport::Tcp, udp)])?;
    drop(udp_held);
    let elsewhere = Ipv4Addr::new(192, 0, 2, 1);
    assert_eq!(free(elsewhere, &listeners, &mut bound), taken);
    passes("opened again after the refusals", &listeners)
}

/// Real sockets: free listeners are opened and closed again, a port another
/// socket holds is refused for its transport alone, and an address that is
/// not this machine's is refused. Nothing listens. The sequence runs on new
/// spare ports up to `ATTEMPTS` times while a step that must pass is
/// refused, and every attempt's diagnosis is reported when none passes.
#[test]
fn listeners_are_opened_without_address_reuse_and_closed_again() {
    let mut refusals = Vec::new();
    for attempt in 1..=ATTEMPTS {
        let (udp, tcp) = spare_ports();
        match listener_sequence(udp, tcp) {
            Ok(()) => return,
            Err(refusal) => refusals.push(format!("attempt {attempt}, {refusal}")),
        }
    }
    panic!(
        "a step that must pass was refused on every attempt:\n{}",
        refusals.join("\n")
    );
}
