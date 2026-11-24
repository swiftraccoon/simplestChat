//! The job-local PostgreSQL relay of a disposable local CI job.
//!
//! act publishes a service's port on the owned engine's bridge gateway, which
//! the job container reaches as `host.docker.internal`, while the database
//! tests admit only a loopback address. The relay listens on
//! `127.0.0.1:PORT` and forwards every connection to the same port of that
//! gateway, which must be private. `build/ci-local-postgres.sh` starts it in
//! the background after its own disposable-job guards; it relays until the
//! job container ends.

use super::engine::reference_private;
use std::{
    convert::Infallible,
    fs::OpenOptions,
    future::Future,
    io::{self, Write},
    net::{Ipv4Addr, SocketAddr, SocketAddrV4},
    path::Path,
    process::ExitCode,
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{
        TcpListener, TcpSocket, TcpStream,
        tcp::{OwnedReadHalf, OwnedWriteHalf},
    },
};

const USAGE: &str =
    "usage: cargo run --locked -q -p simplestchat-ci -- local-postgres-relay PORT READY_FILE";
/// The name the job container resolves to the engine's bridge gateway.
const GATEWAY: &str = "host.docker.internal";
/// The most bytes one read forwards.
const CHUNK: usize = 64 * 1024;
/// How many connections may wait to be accepted.
const BACKLOG: u32 = 32;
/// How long the relay pauses after a failed accept, such as one for which
/// no descriptor is left.
const ACCEPT_PAUSE: Duration = Duration::from_secs(1);

/// How long the relay waits: for the gateway to accept a connection, and
/// for one direction of a connection to carry anything before both close.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Deadlines {
    /// The most time a connection to the gateway may take.
    pub connect: Duration,
    /// The most time one direction may stay silent.
    pub idle: Duration,
}

impl Deadlines {
    /// The relay's own deadlines.
    pub(super) const RELAY: Self = Self {
        connect: Duration::from_secs(10),
        idle: Duration::from_secs(120),
    };
}

/// Why the relay did not start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The gateway name has no IPv4 address.
    Unresolved,
    /// The gateway's address is not private.
    Public,
    /// The loopback port cannot be bound and listened on.
    Listener,
    /// The ready file cannot be created and written.
    Readiness,
}

impl Refusal {
    /// The fixed words the relay prints for this refusal.
    pub(super) fn words(self) -> &'static str {
        match self {
            Self::Unresolved => "The CI service gateway cannot be resolved",
            Self::Public => "The CI service gateway must be private",
            Self::Listener => "The job-local PostgreSQL relay cannot listen on its port",
            Self::Readiness => "The job-local PostgreSQL relay cannot record its readiness",
        }
    }
}

/// Run `local-postgres-relay PORT READY_FILE`. Other arguments exit with
/// status 2 and the usage; a refusal exits with status 1 and its words.
/// Once `READY_FILE` holds `ready`, the relay serves until the process ends
/// and this never returns.
pub(crate) fn run(arguments: &[String]) -> anyhow::Result<ExitCode> {
    let [port, ready] = arguments else {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    let Some(port) = parse_port(port) else {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let started = runtime.block_on(async {
        let addresses = tokio::net::lookup_host((GATEWAY, port)).await;
        let gateway = gateway(addresses.map_err(|_| Refusal::Unresolved)?)?;
        let listener = listen(port).map_err(|_| Refusal::Listener)?;
        record_ready(Path::new(ready)).map_err(|_| Refusal::Readiness)?;
        serve(listener, SocketAddrV4::new(gateway, port), Deadlines::RELAY).await
    });
    match started {
        Ok(never) => match never {},
        Err(refusal) => {
            eprintln!("{}", refusal.words());
            Ok(ExitCode::from(1))
        }
    }
}

/// A port spelled as decimal digits alone, from 1 through 65535.
pub(super) fn parse_port(text: &str) -> Option<u16> {
    let digits = !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit());
    digits
        .then(|| text.parse::<u16>().ok())
        .flatten()
        .filter(|port| *port != 0)
}

/// The first IPv4 address among `addresses`, which must be private as
/// Python's `ipaddress` judged it (the launcher's rule for the gateway).
pub(super) fn gateway(
    addresses: impl IntoIterator<Item = SocketAddr>,
) -> Result<Ipv4Addr, Refusal> {
    let first = addresses.into_iter().find_map(|address| match address {
        SocketAddr::V4(address) => Some(*address.ip()),
        SocketAddr::V6(_) => None,
    });
    let first = first.ok_or(Refusal::Unresolved)?;
    if reference_private(first) {
        Ok(first)
    } else {
        Err(Refusal::Public)
    }
}

/// A listener on `127.0.0.1:port` with address reuse and a backlog of 32.
/// Must be called within a Tokio runtime.
pub(super) fn listen(port: u16) -> io::Result<TcpListener> {
    let socket = TcpSocket::new_v4()?;
    socket.set_reuseaddr(true)?;
    socket.bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))?;
    socket.listen(BACKLOG)
}

/// Create `path`, which must not exist yet, holding `ready` and a newline.
pub(super) fn record_ready(path: &Path) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(b"ready\n")
}

/// Accept connections on `listener` forever and relay each to `upstream`
/// in its own task; a failed accept pauses for a second.
pub(super) async fn serve(
    listener: TcpListener,
    upstream: SocketAddrV4,
    deadlines: Deadlines,
) -> Result<Infallible, Refusal> {
    loop {
        match listener.accept().await {
            Ok((client, _)) => {
                tokio::spawn(relay(client, TcpStream::connect(upstream), deadlines));
            }
            Err(_) => tokio::time::sleep(ACCEPT_PAUSE).await,
        }
    }
}

/// Relay `client` to the stream `connect` opens within the connect
/// deadline. Each direction is copied until its end, which half-closes the
/// other side; a failure or a direction silent for the idle deadline closes
/// both. Without a stream in time, `client` is closed.
pub(super) async fn relay(
    client: TcpStream,
    connect: impl Future<Output = io::Result<TcpStream>>,
    deadlines: Deadlines,
) {
    let Ok(Ok(upstream)) = tokio::time::timeout(deadlines.connect, connect).await else {
        return;
    };
    let (from_client, to_client) = client.into_split();
    let (from_upstream, to_upstream) = upstream.into_split();
    let relayed = tokio::try_join!(
        transfer(from_client, to_upstream, deadlines.idle),
        transfer(from_upstream, to_client, deadlines.idle),
    );
    // Both streams close here, whichever way the relay ended.
    drop(relayed);
}

/// Copy `from` to `to` until `from` ends, then end `to`'s writing side.
async fn transfer(
    mut from: OwnedReadHalf,
    mut to: OwnedWriteHalf,
    idle: Duration,
) -> io::Result<()> {
    let mut buffer = vec![0; CHUNK];
    loop {
        let read = tokio::time::timeout(idle, from.read(&mut buffer)).await;
        let read = read.map_err(|_| io::Error::from(io::ErrorKind::TimedOut))??;
        if read == 0 {
            return to.shutdown().await;
        }
        to.write_all(&buffer[..read]).await?;
    }
}

#[cfg(test)]
#[path = "postgres_relay_tests.rs"]
mod tests;
