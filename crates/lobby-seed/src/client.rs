//! The only five requests the seeding transaction makes, each on its own
//! connection to the private loopback backend: no proxy, no redirect, no
//! other host. An answer's body is kept only when the request succeeded.

use crate::json::{self, Json};
use crate::response::{self, Fault, Incoming, Stop};
use crate::{COOKIE, DEADLINE, FAILED, Limits, MAX_RESPONSE, UNAVAILABLE, cookie};
use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

const TOO_LARGE: &str = "api_response_too_large";
const NO_COOKIE: &str = "refresh_cookie_missing";
const NOT_ENDED: &str = "refresh_session_cleanup_failed";

/// One of the five requests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Login,
    Register,
    Mine,
    Create,
    Logout,
}

impl Operation {
    fn line(self) -> (&'static str, &'static str) {
        match self {
            Operation::Login => ("POST", "/api/auth/login"),
            Operation::Register => ("POST", "/api/auth/register"),
            Operation::Mine => ("GET", "/api/rooms/mine"),
            Operation::Create => ("POST", "/api/rooms"),
            Operation::Logout => ("POST", "/api/auth/logout"),
        }
    }
}

/// A connection read under the operation's two limits.
struct Connection {
    stream: TcpStream,
    request: Duration,
    until: Instant,
}

impl Incoming for Connection {
    fn read(&mut self, buffer: &mut [u8]) -> Result<usize, Stop> {
        let wait = slice(self.request, self.until).ok_or(Stop::Deadline)?;
        let limited = self.stream.set_read_timeout(Some(wait));
        limited.map_err(|_| Stop::Broken)?;
        loop {
            match self.stream.read(buffer) {
                Ok(count) => return Ok(count),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(_) if Instant::now() >= self.until => return Err(Stop::Deadline),
                Err(_) => return Err(Stop::Broken),
            }
        }
    }
}

/// How long one step may wait: the request's own limit, or less when the
/// whole operation ends sooner; nothing once it has ended.
fn slice(request: Duration, until: Instant) -> Option<Duration> {
    let left = until.saturating_duration_since(Instant::now());
    (!left.is_zero()).then(|| request.min(left))
}

/// The session the transaction holds with the backend.
pub(crate) struct Api<'a> {
    backend: SocketAddr,
    origin: &'a str,
    request: Duration,
    pub(crate) until: Instant,
    pub(crate) token: Option<String>,
    pub(crate) authenticated: bool,
    cookie: Option<String>,
}

impl<'a> Api<'a> {
    pub(crate) fn new(backend: SocketAddr, origin: &'a str, limits: &Limits) -> Self {
        Self {
            backend,
            origin,
            request: limits.request,
            until: limits.until,
            token: None,
            authenticated: false,
            cookie: None,
        }
    }

    /// Whether a refresh session is held that can still be ended.
    pub(crate) fn has_cookie(&self) -> bool {
        self.cookie.is_some()
    }

    fn head(&self, operation: Operation, body: Option<&str>) -> String {
        let (method, path) = operation.line();
        let mut head = format!(
            "{method} {path} HTTP/1.1\r\nHost: {}\r\nOrigin: {}\r\nAccept: application/json\r\n",
            self.backend, self.origin
        );
        if let Some(token) = &self.token {
            head.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        if let Some(value) = &self.cookie {
            head.push_str(&format!("Cookie: {COOKIE}={value}\r\n"));
        }
        if body.is_some() {
            head.push_str("Content-Type: application/json\r\n");
        }
        if method == "POST" {
            let length = body.map_or(0, str::len);
            head.push_str(&format!("Content-Length: {length}\r\n"));
        }
        head.push_str("Connection: close\r\n\r\n");
        head
    }

    /// Make one request and return its status with the decoded body of a
    /// successful answer, if it has one.
    pub(crate) fn exchange(
        &mut self,
        operation: Operation,
        body: Option<&str>,
    ) -> Result<(u16, Option<Json>), &'static str> {
        let wait = slice(self.request, self.until).ok_or(DEADLINE)?;
        let connected = TcpStream::connect_timeout(&self.backend, wait);
        let mut stream = connected.map_err(|_| UNAVAILABLE)?;
        let wait = slice(self.request, self.until).ok_or(DEADLINE)?;
        let limited = stream.set_write_timeout(Some(wait));
        limited.map_err(|_| UNAVAILABLE)?;
        let mut request = self.head(operation, body).into_bytes();
        request.extend_from_slice(body.unwrap_or_default().as_bytes());
        let sent = stream.write_all(&request);
        sent.map_err(|_| UNAVAILABLE)?;
        let mut connection = Connection {
            stream,
            request: self.request,
            until: self.until,
        };
        let code = |fault: Fault| match fault {
            Fault::Unavailable => UNAVAILABLE,
            Fault::Malformed => FAILED,
            Fault::Deadline => DEADLINE,
            Fault::TooLarge => TOO_LARGE,
        };
        let answer = response::answer(&mut connection, MAX_RESPONSE).map_err(code)?;
        // The head's cookies count before its body is judged.
        cookie::apply(&mut self.cookie, &answer.cookies);
        let body = answer.body.map_err(code)?;
        let decoded = match body.is_empty() {
            true => None,
            false => Some(json::parse(&body)?),
        };
        Ok((answer.status, decoded))
    }

    /// End the refresh session, when there is or should be one. `None` says
    /// nothing was held.
    pub(crate) fn logout(&mut self) -> Result<Option<bool>, &'static str> {
        if !self.authenticated && !self.has_cookie() {
            return Ok(None);
        }
        if !self.has_cookie() {
            return Err(NO_COOKIE);
        }
        let (status, _) = self.exchange(Operation::Logout, None)?;
        match status == 204 && !self.has_cookie() {
            true => Ok(Some(true)),
            false => Err(NOT_ENDED),
        }
    }
}
