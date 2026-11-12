//! Every recorded run of the earlier seeding helper that reached, or tried
//! to reach, the backend, replayed over real loopback connections.

use crate::testing::{
    Action, Backend, RECORDED, STRICTER, actions, concrete, kept, requests, stricter,
};
use crate::{Limits, Owner, Report, authentication, owned_lobby, parse, seed, token_shape};
use serde_json::Value;
use std::time::{Duration, Instant};

fn recorded() -> Value {
    serde_json::from_str(RECORDED).unwrap()
}

fn owner() -> Owner {
    Owner {
        email: "owner@chat.example.test".to_owned(),
        password: concrete("<password A>"),
    }
}

fn limits() -> Limits {
    Limits {
        request: Duration::from_millis(600),
        farewell: Duration::from_secs(5),
        until: Instant::now() + Duration::from_secs(30),
    }
}

/// What a recorded report says the transaction did.
fn reported(report: &Value) -> Report {
    let failure = report["failure"].as_str();
    Report {
        owner_created: report["ownerCreated"] == true,
        room_created: report["roomCreated"] == true,
        existing_settings_preserved: report["existingSettingsPreserved"] == true,
        refresh_session_revoked: report["refreshSessionRevoked"].as_bool(),
        failure: failure.map(|code| &*code.to_owned().leak()),
    }
}

/// Each of the 185 recorded runs that got as far as the backend makes the
/// recorded requests, byte for byte in what a recording keeps of them, gets
/// the recorded answers and reports what the earlier helper reported:
/// what was created or preserved, whether the session was ended, and the
/// first failure's code. Fourteen runs end differently on purpose.
#[test]
fn every_recorded_transaction_is_replayed_request_by_request() {
    let recorded = recorded();
    let (mut replayed, mut tightened, mut passed) = (0, 0, 0);
    for scenario in recorded["scenarios"].as_array().unwrap() {
        let name = scenario["name"].as_str().unwrap();
        let script = actions(&recorded, scenario);
        let asked = requests(&recorded, scenario);
        let absent = scenario["backend"] == "refused";
        if script.is_empty() && !absent {
            // The run ended at the caller, a file or the lock.
            continue;
        }
        replayed += 1;
        let origin = asked
            .first()
            .map(|request| request.headers["origin"].clone());
        let origin = origin.unwrap_or_else(|| "https://chat.example.test".to_owned());
        let index = usize::try_from(scenario["report"].as_u64().unwrap()).unwrap();
        let mut expected = reported(&recorded["reports"][index]);
        let mut made = asked.len();
        if let Some(outcome) = stricter(name) {
            tightened += 1;
            assert!(expected.failure != Some(outcome.failure), "{name}");
            expected.failure = Some(outcome.failure);
            expected.refresh_session_revoked = outcome.revoked;
            expected.existing_settings_preserved = outcome.preserved;
            made = outcome.requests;
        }
        let backend = (!absent).then(|| Backend::start(script.clone()));
        let address = backend
            .as_ref()
            .map_or_else(Backend::absent, |backend| backend.address);
        let report = seed(address, &origin, &owner(), &limits());
        let (received, unused) = backend.map_or((Vec::new(), 0), Backend::finish);
        assert_eq!(report, expected, "{name}");
        assert_eq!(
            (received.len(), unused),
            (made, script.len() - made),
            "{name}"
        );
        for (position, (sent, wanted)) in received.iter().zip(&asked).enumerate() {
            // A request is never shown: it carries the owner's credentials.
            // Where this client stops early, its last request is the
            // sign-out instead of the recorded next step.
            let early = made < asked.len() && position + 1 == made && position > 0;
            let same = kept(sent) == *wanted;
            let farewell = (sent.method.as_str(), sent.target.as_str());
            assert!(
                same || (early && farewell == ("POST", "/api/auth/logout")),
                "{name}: request {position} differs"
            );
            assert_eq!(sent.headers["host"], address.to_string(), "{name}");
            assert_eq!(sent.headers["connection"], "close", "{name}");
            let length = sent.headers.get("content-length").map(String::as_str);
            let stated = (sent.method == "POST").then(|| sent.body.len().to_string());
            assert_eq!(length, stated.as_deref(), "{name}");
        }
        passed += usize::from(report.passed());
    }
    assert_eq!((replayed, tightened, passed), (185, STRICTER.len(), 35));
}

/// A token is admitted exactly where the earlier helper admitted it, and a
/// list of rooms holds the lobby exactly where it did.
#[test]
fn tokens_and_room_lists_are_admitted_as_recorded() {
    let recorded = recorded();
    let tokens = recorded["vectors"]["token"].as_array().unwrap();
    assert_eq!(tokens.len(), 18);
    for vector in tokens {
        let token = concrete(vector["token"].as_str().unwrap());
        assert!(token_shape(&token) == vector["admitted"], "{}", token.len());
    }
    let lists = recorded["vectors"]["ownedLobby"].as_array().unwrap();
    assert_eq!(lists.len(), 22);
    for vector in lists {
        let rows = parse(vector["rows"].to_string().as_bytes()).unwrap();
        let result = owned_lobby(Some(&rows));
        let expected = match vector["result"]["owned"].as_bool() {
            Some(owned) => Ok(owned),
            None => Err(vector["result"]["refused"].as_str().unwrap()),
        };
        assert_eq!(result, expected, "{}", vector["rows"]);
    }
    assert_eq!(owned_lobby(None), Err("invalid_owned_rooms"));
    // A run has passed only without a failure and with its session ended.
    let outcome = |failure, revoked| Report {
        failure,
        refresh_session_revoked: revoked,
        ..Report::default()
    };
    assert!(outcome(None, Some(true)).passed());
    for (failure, revoked) in [
        (None, None),
        (None, Some(false)),
        (Some("owner_login_failed"), Some(true)),
        (Some("owner_login_failed"), None),
    ] {
        assert!(
            !outcome(failure, revoked).passed(),
            "{failure:?} {revoked:?}"
        );
    }
    // An identity is 32 hexadecimal digits in its five usual groups.
    let answer = |identity: &str| {
        let token = "t".repeat(crate::TOKEN_CHARACTERS);
        let text = format!(r#"{{"token":"{token}","user":{{"id":"{identity}","email":"a"}}}}"#);
        authentication(Some(&parse(text.as_bytes()).unwrap()), "a").map(str::len)
    };
    assert_eq!(answer("01234567-89ab-cdef-0123-456789ABCDEF"), Ok(43));
    for identity in [
        "0123456g-89ab-cdef-0123-456789abcdef",
        "01234567-89ab-cdef-0123-456789abcde",
        "01234567-89ab-cdef-0123-456789abcdef0",
        "0123456-789ab-cdef-0123-456789abcdef",
        "01234567-89ab-cdef-0123456789abcdef",
        "01234567-89ab-cdef-0123-4567-89abcdef",
        "01234567_89ab_cdef_0123_456789abcdef",
    ] {
        assert_eq!(
            answer(identity),
            Err("invalid_owner_identity"),
            "{identity}"
        );
    }
    assert_eq!(
        authentication(None, "a"),
        Err("invalid_authentication_response")
    );
}

/// An operation that has run out of time says so at whichever step it is,
/// makes no further request, and still ends the session within the time
/// that has of its own.
#[test]
fn an_operation_out_of_time_stops_and_still_ends_its_session() {
    let recorded = recorded();
    let scenarios = recorded["scenarios"].as_array().unwrap();
    let usual = scenarios
        .iter()
        .find(|scenario| scenario["name"] == "an existing owner and lobby are preserved");
    let script = actions(&recorded, usual.unwrap());
    // Already over before the first request: nothing is asked.
    let backend = Backend::start(script.clone());
    let late = Limits {
        until: Instant::now(),
        ..limits()
    };
    let report = seed(
        backend.address,
        "https://chat.example.test",
        &owner(),
        &late,
    );
    assert_eq!(backend.finish(), (Vec::new(), 3));
    let expected = Report {
        failure: Some("operation_deadline"),
        ..Report::default()
    };
    assert_eq!(report, expected);
    // Over while the room list stays silent: the wait ends with the
    // operation, not with the request's own limit, and the session is ended.
    let script = vec![script[0].clone(), Action::Hang, script[2].clone()];
    let backend = Backend::start(script);
    let short = Limits {
        request: Duration::from_secs(20),
        until: Instant::now() + Duration::from_millis(700),
        ..limits()
    };
    let began = Instant::now();
    let report = seed(
        backend.address,
        "https://chat.example.test",
        &owner(),
        &short,
    );
    assert!(began.elapsed() < Duration::from_secs(10));
    let (received, unused) = backend.finish();
    assert_eq!((received.len(), unused), (3, 0));
    let expected = Report {
        failure: Some("operation_deadline"),
        refresh_session_revoked: Some(true),
        ..Report::default()
    };
    assert_eq!(report, expected);
    // A sign-out that outlasts its own time leaves the session unrevoked.
    let script = actions(&recorded, usual.unwrap());
    let backend = Backend::start(vec![script[0].clone(), script[1].clone(), Action::Hang]);
    let brief = Limits {
        request: Duration::from_secs(20),
        farewell: Duration::from_millis(300),
        ..limits()
    };
    let report = seed(
        backend.address,
        "https://chat.example.test",
        &owner(),
        &brief,
    );
    assert_eq!(backend.finish().0.len(), 3);
    let expected = Report {
        existing_settings_preserved: true,
        refresh_session_revoked: Some(false),
        failure: Some("refresh_session_cleanup_failed"),
        ..Report::default()
    };
    assert_eq!(report, expected);
}

/// Only the exact refresh cookie counts: by name, host-only, for the whole
/// site, marked secure, with no lifetime or one in seconds.
#[test]
fn only_the_exact_refresh_cookie_changes_what_is_kept() {
    use crate::cookie::{Change, apply, change};
    let set = |value: &str| Some(Change::Set(value.to_owned()));
    let cases = [
        ("__Host-refresh_token=v; Secure; Path=/", set("v")),
        (
            "__Host-refresh_token=v; Path=/; HttpOnly; Secure; SameSite=Strict; Max-Age=1",
            set("v"),
        ),
        (
            " __Host-refresh_token = v ; SECURE ; PATH = / ; MAX-AGE = 9 ",
            set("v"),
        ),
        ("__Host-refresh_token=v;Secure;Path=/", set("v")),
        ("__Host-refresh_token=v; Secure=no; Path=/", set("v")),
        ("__Host-refresh_token=; Secure; Path=/", set("")),
        ("__Host-refresh_token=a=b; Secure; Path=/", set("a=b")),
        ("__Host-refresh_token=\"v\"; Secure; Path=/", set("\"v\"")),
        (
            "__Host-refresh_token=v; Secure; Path=/; Max-Age=+7",
            set("v"),
        ),
        (
            "__Host-refresh_token=v; Secure; Path=/; Max-Age=0",
            Some(Change::Clear),
        ),
        (
            "__Host-refresh_token=; Secure; Path=/; Max-Age=-5",
            Some(Change::Clear),
        ),
        ("__Host-refresh_token=v; Secure", None),
        ("__Host-refresh_token=v; Path=/", None),
        ("__Host-refresh_token=v; Secure; Path=/api", None),
        ("__Host-refresh_token=v; Secure; Path=\"/\"", None),
        (
            "__Host-refresh_token=v; Secure; Path=/; Domain=127.0.0.1",
            None,
        ),
        (
            "__Host-refresh_token=v; Secure; Path=/; Expires=Fri, 01 Jan 2100 00:00:00 GMT",
            None,
        ),
        (
            "__Host-refresh_token=; Secure; Path=/; Expires=Thu, 01 Jan 1970 00:00:00 GMT",
            None,
        ),
        ("__Host-refresh_token=v; Secure; Path=/; Max-Age=soon", None),
        ("__Host-refresh_token=v; Secure; Path=/; Max-Age=", None),
        (
            "__Host-refresh_token=v; Secure; Path=/; Max-Age=0; Domain=127.0.0.1",
            None,
        ),
        ("__host-refresh_token=v; Secure; Path=/", None),
        ("refresh_token=v; Secure; Path=/", None),
        (
            "theme=dark; Path=/, __Host-refresh_token=v; Secure; Path=/",
            None,
        ),
        ("__Host-refresh_token", None),
        ("", None),
    ];
    for (header, expected) in cases {
        assert_eq!(change(header), expected, "{header}");
    }
    // A value is kept up to 1024 bytes; a longer one can still clear.
    let (largest, larger) = ("v".repeat(1024), "v".repeat(1025));
    let header =
        |value: &str, rest: &str| format!("__Host-refresh_token={value}; Secure; Path=/{rest}");
    assert_eq!(change(&header(&largest, "")), set(&largest));
    assert_eq!(change(&header(&larger, "")), None);
    assert_eq!(change(&header(&larger, "; Max-Age=0")), Some(Change::Clear));
    // One response: a clearing header acts first wherever it stands, the
    // last setting header wins, and anything else leaves the cookie alone.
    let (first, second, clear) = (header("a", ""), header("b", ""), header("", "; Max-Age=0"));
    let after = |kept: Option<&str>, headers: &[&String]| {
        let mut kept = kept.map(str::to_owned);
        let headers: Vec<String> = headers.iter().map(|header| (*header).clone()).collect();
        apply(&mut kept, &headers);
        kept
    };
    assert_eq!(after(None, &[&first, &second]).as_deref(), Some("b"));
    assert_eq!(after(Some("old"), &[&clear, &first]).as_deref(), Some("a"));
    assert_eq!(after(Some("old"), &[&first, &clear]).as_deref(), Some("a"));
    assert_eq!(after(Some("old"), &[&clear]), None);
    assert_eq!(
        after(Some("old"), &[&"theme=dark; Path=/".to_owned()]).as_deref(),
        Some("old")
    );
    assert_eq!(after(Some("old"), &[]).as_deref(), Some("old"));
}

/// A connection that delivers scripted pieces and then ends or fails.
struct Pieces(std::collections::VecDeque<Result<Vec<u8>, crate::response::Stop>>);

impl crate::response::Incoming for Pieces {
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, crate::response::Stop> {
        match self.0.pop_front() {
            None => Ok(0),
            Some(Err(stop)) => Err(stop),
            Some(Ok(piece)) => {
                buffer[..piece.len()].copy_from_slice(&piece);
                Ok(piece.len())
            }
        }
    }
}

/// An answer is read the same however its bytes arrive; its body is taken
/// only from a success, within the stated length or the allowance, and a
/// head stands, with its cookies, when the body cannot be taken.
#[test]
fn an_answer_is_framed_within_bounds_and_keeps_its_head() {
    use crate::response::{Answer, Fault, Stop, answer};
    type Outcome = Result<(u16, Vec<String>, Result<Vec<u8>, Fault>), Fault>;
    let read = |pieces: Vec<Result<Vec<u8>, Stop>>, most: usize| -> Outcome {
        let mut source = Pieces(pieces.into());
        answer(&mut source, most).map(
            |Answer {
                 status,
                 cookies,
                 body,
             }| (status, cookies, body),
        )
    };
    let whole = |text: &str, most: usize| read(vec![Ok(text.as_bytes().to_vec())], most);
    let ok = |status: u16, body: &str| Ok((status, Vec::new(), Ok(body.as_bytes().to_vec())));
    let text =
        "HTTP/1.1 200 OK\r\nSet-Cookie: a=1\r\nset-cookie: b=2\r\nContent-Length: 5\r\n\r\nhello";
    let expected: Outcome = Ok((
        200,
        vec!["a=1".to_owned(), "b=2".to_owned()],
        Ok(b"hello".to_vec()),
    ));
    assert_eq!(whole(text, 5), expected);
    let single = text.bytes().map(|byte| Ok(vec![byte])).collect();
    assert_eq!(read(single, 5), expected);
    // Lengths: shorter than stated is taken as it came, longer is cut, one
    // byte over the allowance is too large, and no length reads to the end.
    assert_eq!(
        whole("HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nhello", 9),
        ok(200, "hello")
    );
    assert_eq!(
        whole("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nhello", 9),
        ok(200, "he")
    );
    assert_eq!(
        whole("HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\nhello", 9),
        ok(200, "")
    );
    let large: Outcome = Ok((200, Vec::new(), Err(Fault::TooLarge)));
    assert_eq!(
        whole("HTTP/1.1 200 OK\r\nContent-Length: 6\r\n\r\nhello!", 5),
        large
    );
    assert_eq!(whole("HTTP/1.0 200 OK\r\n\r\nhello", 5), ok(200, "hello"));
    assert_eq!(whole("HTTP/1.0 200 OK\r\n\r\nhello!", 5), large);
    assert_eq!(
        whole(
            "HTTP/1.1 200 OK\r\nContent-Length: 5\r\ncontent-length: 5\r\n\r\nhello",
            5
        ),
        ok(200, "hello")
    );
    // Only a success has a body, and never one that has none by its status.
    assert_eq!(
        whole("HTTP/1.1 500 Oops\r\nContent-Length: 5\r\n\r\nhello", 5),
        ok(500, "")
    );
    assert_eq!(
        whole("HTTP/1.1 302 Found\r\nLocation: /x\r\n\r\nhello", 5),
        ok(302, "")
    );
    assert_eq!(whole("HTTP/1.1 199 Early\r\n\r\nhello", 5), ok(199, ""));
    assert_eq!(whole("HTTP/1.1 300 Many\r\n\r\nhello", 5), ok(300, ""));
    assert_eq!(whole("HTTP/1.1 299 Odd\r\n\r\nhello", 5), ok(299, "hello"));
    assert_eq!(
        whole(
            "HTTP/1.1 204 No Content\r\nContent-Length: 5\r\n\r\nhello",
            5
        ),
        ok(204, "")
    );
    // Chunks: sizes in either case with extensions, ended by a zero size.
    let chunked = "HTTP/1.1 200 OK\r\nTransfer-Encoding: Chunked\r\nContent-Length: 1\r\n\r\n";
    let body = |chunks: &str, most: usize| whole(&format!("{chunked}{chunks}"), most);
    assert_eq!(
        body("3\r\nhel\r\n2;note=1\r\nlo\r\n0\r\n\r\n", 5),
        ok(200, "hello")
    );
    assert_eq!(
        body("A\r\n0123456789\r\n0\r\n\r\n", 10),
        ok(200, "0123456789")
    );
    assert_eq!(body("a\r\n0123456789\r\n1\r\n!\r\n0\r\n\r\n", 10), large);
    assert_eq!(body("b\r\n0123456789!\r\n0\r\n\r\n", 10), large);
    let broken: Outcome = Ok((200, Vec::new(), Err(Fault::Malformed)));
    for chunks in [
        "3\r\nhel\r\n",
        "3\r\nhe",
        "3\r\nhel--0\r\n\r\n",
        "zz\r\nhel\r\n0\r\n\r\n",
        "\r\n",
        "3",
    ] {
        assert_eq!(body(chunks, 5), broken, "{chunks:?}");
    }
    // The head: nothing at all is an absent backend, a head that breaks off
    // or is not a head is malformed, and so are lengths that disagree or are
    // not plain numbers.
    assert_eq!(read(Vec::new(), 5), Err(Fault::Unavailable));
    assert_eq!(read(vec![Err(Stop::Broken)], 5), Err(Fault::Unavailable));
    assert_eq!(read(vec![Err(Stop::Deadline)], 5), Err(Fault::Deadline));
    assert_eq!(
        read(vec![Ok(b"HTTP/1.1 2".to_vec()), Err(Stop::Broken)], 5),
        Err(Fault::Unavailable)
    );
    assert_eq!(
        read(vec![Ok(b"HTTP/1.1 2".to_vec()), Err(Stop::Deadline)], 5),
        Err(Fault::Deadline)
    );
    for head in [
        "HTTP/1.1 200 OK\r\n",
        "HTTP/1.1 2",
        "SSH-2.0-fixture\r\n\r\n",
        "HTTP/1.1 200 OK\r\nX: a\r\n b\r\n\r\n",
        "HTTP/1.1 200 OK\r\nContent-Length: 5\r\nContent-Length: 6\r\n\r\nhello",
        "HTTP/1.1 200 OK\r\nContent-Length: +5\r\n\r\nhello",
        "HTTP/1.1 200 OK\r\nContent-Length: x\r\n\r\nhello",
        "HTTP/1.1 200 OK\r\nContent-Length: \r\n\r\nhello",
    ] {
        assert_eq!(whole(head, 5), Err(Fault::Malformed), "{head:?}");
    }
    // What is not a head is given up at once, not waited on for more.
    let foreign = vec![Ok(b"SSH-2.0-fixture\r\n".to_vec()), Err(Stop::Deadline)];
    assert_eq!(read(foreign, 5), Err(Fault::Malformed));
    // A cookie header that is not text is not a head this client reads.
    let unreadable = b"HTTP/1.1 200 OK\r\nSet-Cookie: \xff\r\n\r\n".to_vec();
    assert_eq!(read(vec![Ok(unreadable)], 5), Err(Fault::Malformed));
    // A head may take 64 KiB: one that goes on is given up before more is
    // read, here before the piece that would report the deadline.
    let mut endless = vec![Ok(b"HTTP/1.1 200 OK\r\nX: ".to_vec())];
    endless.extend(vec![Ok(vec![b'p'; 8192]); 8]);
    endless.push(Err(Stop::Deadline));
    assert_eq!(read(endless.clone(), 5), Err(Fault::Malformed));
    drop(endless.remove(8));
    assert_eq!(read(endless, 5), Err(Fault::Deadline));
    let padded = format!(
        "HTTP/1.1 200 OK\r\nX: {}\r\nContent-Length: 2\r\n\r\nhi",
        "p".repeat(60_000)
    );
    // A head that stops inside a header is not yet one.
    assert_eq!(whole(&padded[..8000], 5), Err(Fault::Malformed));
    let pieces = padded
        .as_bytes()
        .chunks(8000)
        .map(|piece| Ok(piece.to_vec()))
        .collect();
    assert_eq!(read(pieces, 5), ok(200, "hi"));
    // A body that fails leaves the head and its cookies standing.
    let head = b"HTTP/1.1 200 OK\r\nSet-Cookie: a=1\r\nContent-Length: 5\r\n\r\nhe".to_vec();
    let kept = |fault: Fault| -> Outcome { Ok((200, vec!["a=1".to_owned()], Err(fault))) };
    assert_eq!(
        read(vec![Ok(head.clone()), Err(Stop::Broken)], 5),
        kept(Fault::Malformed)
    );
    assert_eq!(
        read(vec![Ok(head), Err(Stop::Deadline)], 5),
        kept(Fault::Deadline)
    );
}

/// Text is one JSON value or it is refused, and an object that repeats a key
/// is refused as such once it is complete, at any depth.
#[test]
fn json_is_one_value_without_repeated_keys() {
    use crate::{DUPLICATE, INVALID, Json};
    let object = parse(br#" {"a":[1,-2.5e3,true,null,"x"],"b":{"c":false}} "#).unwrap();
    let list = Json::Array(vec![
        Json::Number,
        Json::Number,
        Json::Bool(true),
        Json::Null,
        Json::String("x".to_owned()),
    ]);
    assert_eq!(object.get("a"), Some(&list));
    assert_eq!(
        object.get("b").and_then(|inner| inner.get("c")),
        Some(&Json::Bool(false))
    );
    assert_eq!(
        (object.get("c"), list.get("a"), list.text()),
        (None, None, None)
    );
    assert_eq!(parse(br#""text""#).unwrap().text(), Some("text"));
    for text in [
        r#"{"a":1,"a":2}"#,
        r#"{"a":1,"b":{"c":1,"c":1}}"#,
        r#"[{"a":1,"a":1}]"#,
        r#"{"a":{"b":1,"b":2}} trailing"#,
        r#"{"a":1,"a":2}"#,
    ] {
        assert_eq!(parse(text.as_bytes()), Err(DUPLICATE), "{text}");
    }
    for text in [
        "",
        " ",
        "{",
        r#"{"a":1,"a":2"#,
        r#"{"a":1,"a":2,"#,
        r#"{"a":1} x"#,
        "NaN",
        "1e999",
        "'a'",
        "\u{feff}{}",
        r#""\ud800""#,
    ] {
        assert_eq!(parse(text.as_bytes()), Err(INVALID), "{text}");
    }
    assert_eq!(parse(b"\"\xff\""), Err(INVALID));
    assert_eq!(parse("[".repeat(200).as_bytes()), Err(INVALID));
}
