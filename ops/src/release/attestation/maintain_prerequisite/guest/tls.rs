//! One fixed metrics TLS exchange, with separate actual served and verified chains.
//! Endpoint/CID/IPAM authority and retained-material custody belong to the caller.

use super::{
    Control, Error, Result,
    materials::{self, Materials},
};
use openssl::{
    ssl::{
        ErrorCode, Ssl, SslContext, SslMethod, SslOptions, SslStream, SslVerifyMode, SslVersion,
    },
    x509::{X509, X509VerifyResult, store::X509StoreBuilder},
};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    net::{AddressFamily, SocketFlags, SocketType},
};
use std::{
    io::{self, Read, Write},
    net::{Ipv4Addr, SocketAddrV4, TcpStream},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

const REQUEST: &[u8] =
    b"GET /metrics HTTP/1.1\r\nHost: maintain.localhost\r\nConnection: close\r\n\r\n";
const MAX_BODY: usize = 65536;
const MAX_LINE: usize = 65536;
const MAX_HEADER: usize = 101 * 65536;
const MAX_WIRE: usize = MAX_HEADER + MAX_BODY * 64;

pub(super) struct TlsProof {
    verified: Vec<Vec<u8>>,
    served: Vec<Vec<u8>>,
    pub verified_hashes: Vec<String>,
    pub served_hashes: Vec<String>,
    pub metrics_not_found: bool,
}
impl TlsProof {
    pub fn bind(&self, materials: &Materials) -> Result<()> {
        if self.verified != materials.der
            || ![2, 3].contains(&self.served.len())
            || self.served != materials.der[..self.served.len()]
            || self.verified_hashes != materials.der_hashes
            || self.served_hashes != materials.der_hashes[..self.served.len()]
            || !self.metrics_not_found
        {
            return Err(Error::Refused);
        }
        Ok(())
    }
}

/// Arc shares only the reachable original socket, never a duplicated descriptor.
/// An SSL/BIO construction failure cannot hide that original from explicit close.
#[derive(Clone)]
struct Socket(Arc<Mutex<Option<TcpStream>>>);
impl Read for Socket {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let held = self
            .0
            .lock()
            .map_err(|_| io::Error::other("socket owner"))?;
        held.as_ref()
            .ok_or_else(|| io::Error::other("closed socket"))?
            .read(bytes)
    }
}
impl Write for Socket {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let held = self
            .0
            .lock()
            .map_err(|_| io::Error::other("socket owner"))?;
        held.as_ref()
            .ok_or_else(|| io::Error::other("closed socket"))?
            .write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl Socket {
    fn close(&self) -> Result<()> {
        let poisoned = self.0.is_poisoned();
        let mut held = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let closed = held
            .take()
            .map(nix::unistd::close)
            .transpose()
            .map_err(|_| Error::Close);
        closed?;
        if poisoned {
            return Err(Error::Close);
        }
        Ok(())
    }
    fn wait(&self, flags: PollFlags, control: &mut Control, end: Instant) -> Result<()> {
        fence(control, end)?;
        let held = self.0.lock().map_err(|_| Error::Io)?;
        let stream = held.as_ref().ok_or(Error::Close)?;
        let mut fds = [PollFd::new(stream, flags)];
        let left = end
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(20));
        match poll(
            &mut fds,
            Some(&Timespec {
                tv_sec: 0,
                tv_nsec: i64::from(left.subsec_nanos()),
            }),
        ) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(_) => return Err(Error::Io),
        }
        fence(control, end)
    }
}
fn fence(control: &mut Control, end: Instant) -> Result<()> {
    control.check()?;
    if Instant::now() >= end {
        return Err(Error::Deadline);
    }
    Ok(())
}
fn wanted(error: &openssl::ssl::Error) -> Option<PollFlags> {
    match error.code() {
        ErrorCode::WANT_READ => Some(PollFlags::IN),
        ErrorCode::WANT_WRITE => Some(PollFlags::OUT),
        _ => None,
    }
}
fn client(root: X509) -> Result<SslContext> {
    // A new bare context has no default paths or environment-provided anchors.
    let mut store = X509StoreBuilder::new().map_err(|_| Error::Io)?;
    store.add_cert(root).map_err(|_| Error::Refused)?;
    let mut context = SslContext::builder(SslMethod::tls_client()).map_err(|_| Error::Io)?;
    context.set_cert_store(store.build());
    context.set_verify(SslVerifyMode::PEER);
    context.set_options(SslOptions::IGNORE_UNEXPECTED_EOF);
    context
        .set_min_proto_version(Some(SslVersion::TLS1_2))
        .map_err(|_| Error::Io)?;
    Ok(context.build())
}

fn chains(ssl: &openssl::ssl::SslRef, root: &X509) -> Result<TlsProof> {
    if ssl.verify_result() != X509VerifyResult::OK {
        return Err(Error::Refused);
    }
    let verified = ssl.verified_chain().ok_or(Error::Refused)?;
    let served = ssl.peer_cert_chain().ok_or(Error::Refused)?;
    if verified.len() != 3 || ![2, 3].contains(&served.len()) {
        return Err(Error::Refused);
    }
    let verified = verified
        .iter()
        .map(|v| v.to_der().map_err(|_| Error::Refused))
        .collect::<Result<Vec<_>>>()?;
    let served = served
        .iter()
        .map(|v| v.to_der().map_err(|_| Error::Refused))
        .collect::<Result<Vec<_>>>()?;
    if verified.len() != 3
        || ![2, 3].contains(&served.len())
        || verified[2] != root.to_der().map_err(|_| Error::Refused)?
        || served != verified[..served.len()]
    {
        return Err(Error::Refused);
    }
    let verified_hashes = verified.iter().map(|v| super::render::digest(v)).collect();
    let served_hashes = served.iter().map(|v| super::render::digest(v)).collect();
    Ok(TlsProof {
        verified,
        served,
        verified_hashes,
        served_hashes,
        metrics_not_found: false,
    })
}

#[derive(Clone, Copy)]
enum Body {
    Length(usize),
    Chunked,
    Close,
}
fn line_bounds(bytes: &[u8], maximum: Option<usize>) -> Result<()> {
    let mut at = 0;
    let mut count = 0;
    while let Some(offset) = bytes[at..].iter().position(|v| *v == b'\n') {
        let end = at + offset + 1;
        if end - at > MAX_LINE {
            return Err(Error::Bounds);
        }
        if matches!(&bytes[at..end], b"\r\n" | b"\n") {
            return Ok(());
        }
        count += 1;
        if maximum.is_some_and(|maximum| count > maximum) {
            return Err(Error::Bounds);
        }
        at = end;
    }
    if bytes.len() - at > MAX_LINE {
        return Err(Error::Bounds);
    }
    Ok(())
}
fn headers(bytes: &[u8]) -> Result<Option<(usize, Body)>> {
    if bytes.len() > MAX_WIRE {
        return Err(Error::Bounds);
    }
    // Python's original100-line header bound includes the blank terminator;
    // the separate status line plus99 fields gives100 nonempty lines here.
    line_bounds(bytes, Some(100))?;
    let mut storage = [httparse::EMPTY_HEADER; 99];
    let mut response = httparse::Response::new(&mut storage);
    let parsed = response.parse(bytes).map_err(|_| Error::Refused)?;
    let httparse::Status::Complete(offset) = parsed else {
        if bytes.len() > MAX_HEADER {
            return Err(Error::Bounds);
        }
        return Ok(None);
    };
    if offset > MAX_HEADER || response.code != Some(404) {
        return Err(Error::Refused);
    }
    let mut length = None;
    let mut chunked = false;
    for header in response.headers {
        if header.name.eq_ignore_ascii_case("content-length") {
            let text = std::str::from_utf8(header.value)
                .map_err(|_| Error::Refused)?
                .trim();
            if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Error::Refused);
            }
            let value = text.parse::<usize>().map_err(|_| Error::Bounds)?;
            if value > MAX_BODY || length.is_some_and(|previous| previous != value) {
                return Err(Error::Bounds);
            }
            length = Some(value);
        }
        if header.name.eq_ignore_ascii_case("transfer-encoding") {
            if chunked || !header.value.eq_ignore_ascii_case(b"chunked") {
                return Err(Error::Refused);
            }
            chunked = true;
        }
    }
    if chunked && length.is_some() {
        return Err(Error::Refused);
    }
    Ok(Some((
        offset,
        if chunked {
            Body::Chunked
        } else if let Some(value) = length {
            Body::Length(value)
        } else {
            Body::Close
        },
    )))
}
fn body_done(raw: &[u8], framing: Body, eof: bool) -> Result<bool> {
    match framing {
        Body::Length(count) => Ok(raw.len() >= count),
        Body::Close => {
            if raw.len() > MAX_BODY {
                return Err(Error::Bounds);
            }
            Ok(eof)
        }
        Body::Chunked => {
            let mut at = 0;
            let mut total = 0usize;
            loop {
                let Some(line) = raw[at..].windows(2).position(|v| v == b"\r\n") else {
                    if raw.len() - at > MAX_LINE {
                        return Err(Error::Bounds);
                    }
                    return if eof { Err(Error::Refused) } else { Ok(false) };
                };
                if line + 2 > MAX_LINE {
                    return Err(Error::Bounds);
                }
                let size = raw[at..at + line]
                    .split(|b| *b == b';')
                    .next()
                    .ok_or(Error::Refused)?;
                if size.is_empty() || !size.iter().all(u8::is_ascii_hexdigit) {
                    return Err(Error::Refused);
                }
                let size = usize::from_str_radix(
                    std::str::from_utf8(size).map_err(|_| Error::Refused)?,
                    16,
                )
                .map_err(|_| Error::Bounds)?;
                at += line + 2;
                if size == 0 {
                    if raw[at..].starts_with(b"\r\n") {
                        return Ok(true);
                    }
                    line_bounds(&raw[at..], None)?;
                    let mut trailer = [httparse::EMPTY_HEADER; 100];
                    return match httparse::parse_headers(&raw[at..], &mut trailer)
                        .map_err(|_| Error::Refused)?
                    {
                        httparse::Status::Complete(_) => Ok(true),
                        httparse::Status::Partial if eof => Err(Error::Refused),
                        httparse::Status::Partial => Ok(false),
                    };
                }
                total = total
                    .checked_add(size)
                    .filter(|v| *v <= MAX_BODY)
                    .ok_or(Error::Bounds)?;
                let end = at
                    .checked_add(size)
                    .and_then(|v| v.checked_add(2))
                    .ok_or(Error::Bounds)?;
                if raw.len() < end {
                    return if eof { Err(Error::Refused) } else { Ok(false) };
                }
                if &raw[end - 2..end] != b"\r\n" {
                    return Err(Error::Refused);
                }
                at = end;
            }
        }
    }
}

fn attempt_end(control: &mut Control) -> Result<Instant> {
    let began = Instant::now();
    control.check()?;
    Ok(control.phase_end().min(
        began
            .checked_add(Duration::from_secs(3))
            .ok_or(Error::Deadline)?,
    ))
}
fn exchange(
    address: SocketAddrV4,
    root: X509,
    control: &mut Control,
    end: Instant,
) -> Result<TlsProof> {
    fence(control, end)?;
    let context = client(root.clone())?;
    let mut ssl = Ssl::new(&context).map_err(|_| Error::Io)?;
    ssl.set_hostname(materials::NAME)
        .map_err(|_| Error::Refused)?;
    ssl.param_mut()
        .set_host(materials::NAME)
        .map_err(|_| Error::Refused)?;
    ssl.set_connect_state();
    fence(control, end)?;
    #[cfg(target_os = "linux")]
    let flags = SocketFlags::NONBLOCK | SocketFlags::CLOEXEC;
    #[cfg(not(target_os = "linux"))]
    let flags = SocketFlags::empty();
    let fd = rustix::net::socket_with(AddressFamily::INET, SocketType::STREAM, flags, None)
        .map_err(|_| Error::Io)?;
    let socket = Socket(Arc::new(Mutex::new(Some(TcpStream::from(fd)))));
    let result = (|| {
        {
            let held = socket.0.lock().map_err(|_| Error::Io)?;
            let stream = held.as_ref().ok_or(Error::Close)?;
            #[cfg(not(target_os = "linux"))]
            {
                // The reachable original is registered before either fallible
                // portability step. Both error paths use the common checked close.
                let flags = rustix::fs::fcntl_getfl(stream).map_err(|_| Error::Io)?;
                rustix::fs::fcntl_setfl(stream, flags | rustix::fs::OFlags::NONBLOCK)
                    .map_err(|_| Error::Io)?;
                let flags = rustix::io::fcntl_getfd(stream).map_err(|_| Error::Io)?;
                rustix::io::fcntl_setfd(stream, flags | rustix::io::FdFlags::CLOEXEC)
                    .map_err(|_| Error::Io)?;
            }
            fence(control, end)?;
            match rustix::net::connect(stream, &address) {
                Ok(()) => {}
                Err(
                    rustix::io::Errno::INPROGRESS
                    | rustix::io::Errno::ALREADY
                    | rustix::io::Errno::INTR,
                ) => {}
                Err(_) => return Err(Error::Io),
            }
        }
        loop {
            socket.wait(PollFlags::OUT, control, end)?;
            let held = socket.0.lock().map_err(|_| Error::Io)?;
            let stream = held.as_ref().ok_or(Error::Close)?;
            if stream.take_error().map_err(|_| Error::Io)?.is_some() {
                return Err(Error::Io);
            }
            match stream.peer_addr() {
                Ok(peer) if peer == std::net::SocketAddr::V4(address) => break,
                Ok(_) => return Err(Error::Refused),
                Err(error) if error.kind() == io::ErrorKind::NotConnected => {}
                Err(_) => return Err(Error::Io),
            }
        }
        fence(control, end)?;
        let mut stream = SslStream::new(ssl, socket.clone()).map_err(|_| Error::Io)?;
        loop {
            fence(control, end)?;
            match stream.connect() {
                Ok(()) => break,
                Err(error) => {
                    if let Some(flags) = wanted(&error) {
                        socket.wait(flags, control, end)?;
                    } else {
                        return Err(Error::Io);
                    }
                }
            }
        }
        fence(control, end)?;
        let mut proof = chains(stream.ssl(), &root)?;
        let mut at = 0;
        while at < REQUEST.len() {
            fence(control, end)?;
            match stream.ssl_write(&REQUEST[at..]) {
                Ok(0) => return Err(Error::Io),
                Ok(count) => at += count,
                Err(error) => {
                    if let Some(flags) = wanted(&error) {
                        socket.wait(flags, control, end)?;
                    } else {
                        return Err(Error::Io);
                    }
                }
            }
        }
        let mut bytes = Vec::new();
        let mut parsed = None;
        loop {
            fence(control, end)?;
            if parsed.is_none() {
                parsed = headers(&bytes)?;
            }
            if let Some((offset, framing)) = parsed
                && body_done(&bytes[offset..], framing, false)?
            {
                break;
            }
            let mut buffer = [0u8; 8192];
            let count = match stream.ssl_read(&mut buffer) {
                Ok(count) => count,
                Err(error) if error.code() == ErrorCode::ZERO_RETURN => 0,
                Err(error) => {
                    if let Some(flags) = wanted(&error) {
                        socket.wait(flags, control, end)?;
                        continue;
                    }
                    // Python's default SSLSocket permits a transport EOF without close_notify.
                    if error.code() == ErrorCode::SYSCALL && error.io_error().is_none() {
                        0
                    } else {
                        return Err(Error::Io);
                    }
                }
            };
            fence(control, end)?;
            if count == 0 {
                let (offset, framing) = parsed.ok_or(Error::Refused)?;
                if !body_done(&bytes[offset..], framing, true)? {
                    return Err(Error::Refused);
                }
                break;
            }
            if bytes.len().checked_add(count).is_none_or(|n| n > MAX_WIRE) {
                return Err(Error::Bounds);
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        proof.metrics_not_found = true;
        fence(control, end)?;
        Ok(proof)
    })();
    // SSL state has left the operation scope before the original socket closes.
    let closed = socket.close();
    settled(result, closed, control, end)
}
fn settled(
    result: Result<TlsProof>,
    closed: Result<()>,
    control: &mut Control,
    end: Instant,
) -> Result<TlsProof> {
    if let Err(error) = &result
        && *error != Error::Io
    {
        control.record(*error);
    }
    if let Err(error) = closed {
        control.cleanup_failed = true;
        control.record(result.as_ref().err().copied().unwrap_or(error));
    }
    let late = control
        .check_after_failure(result.as_ref().err().copied())
        .and_then(|()| {
            if Instant::now() >= end {
                Err(Error::Deadline)
            } else {
                Ok(())
            }
        });
    if let Err(error) = &late {
        // A transport error may be retried only after an on-time checked close.
        // Keep the original cause when its attempt fence has also failed.
        control.record(result.as_ref().err().copied().unwrap_or(*error));
    }
    match result {
        Err(error) => Err(error),
        Ok(proof) => {
            closed?;
            late?;
            Ok(proof)
        }
    }
}

/// Initial issuance probe: root exists before the leaf; binding follows admission of all seven.
pub(super) fn probe_root(ip: Ipv4Addr, root_pem: &[u8], control: &mut Control) -> Result<TlsProof> {
    let end = attempt_end(control)?;
    let roots = materials::certificates(root_pem)?;
    let [root]: [_; 1] = roots.try_into().map_err(|_| Error::Refused)?;
    let result = exchange(SocketAddrV4::new(ip, 443), root, control, end);
    let live = control.check_after_failure(result.as_ref().err().copied());
    result.and_then(|proof| live.map(|()| proof))
}
#[cfg(test)]
#[path = "tls_tests.rs"]
mod tests;
