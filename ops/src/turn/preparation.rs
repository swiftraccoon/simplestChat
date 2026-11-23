//! The relay's first preparation steps, before anything of the relay is
//! written: the IPv4 address it will listen on must be a public one of this
//! machine with its three listeners free, and its shared secret is made
//! once and from then on only checked.

use crate::host::fault::{Fault, Result, require};
use crate::host::{global_ipv4, hexadecimal};
use nix::fcntl::OFlag;
use rustix::fd::OwnedFd;
use rustix::net::{AddressFamily, SocketType};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;

/// A transport the relay listens on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Transport {
    Udp,
    Tcp,
}

/// The relay's listeners, in the order they are opened: STUN and TURN over
/// UDP and over TCP, and TURN over TLS.
pub(super) const LISTENERS: [(Transport, u16); 3] = [
    (Transport::Udp, 3478),
    (Transport::Tcp, 3478),
    (Transport::Tcp, 5349),
];

/// The refusal of an address that is not one public IPv4 address.
pub(super) const NOT_PUBLIC: &str = "A local public IPv4 address is required";
/// The refusal of an address where a listener cannot be opened: the port is
/// taken there, or the address is not this machine's.
pub(super) const TAKEN: &str = "The relay listeners must be free on the public address";
/// The refusal of anything at the secret's path but its private secret.
pub(super) const UNSAFE: &str = "Operational precondition failed";

/// Where the relay's shared secret is kept.
pub(super) const RELAY_FILE: &str = "/etc/simplestchat-turn/secret";
/// How many lowercase hexadecimal digits the secret is.
const DIGITS: usize = 64;

/// The address `text` spells, when it is a globally reachable IPv4 address
/// in its one canonical spelling: four decimal numbers without leading
/// zeros or blanks.
pub(super) fn public(text: &str) -> Result<Ipv4Addr> {
    let address: Ipv4Addr = text.parse().map_err(|_| refused(NOT_PUBLIC))?;
    require(global_ipv4(address), NOT_PUBLIC)?;
    Ok(address)
}

/// Open every listener at `address` through `open`, holding each until all
/// are open, then close them all. The first one `open` cannot open refuses
/// and none after it is tried. Nothing listens or accepts.
pub(super) fn free(
    address: Ipv4Addr,
    listeners: &[(Transport, u16)],
    open: &mut dyn FnMut(Transport, SocketAddrV4) -> std::io::Result<OwnedFd>,
) -> Result<()> {
    let mut held = Vec::with_capacity(listeners.len());
    for &(transport, port) in listeners {
        let socket = open(transport, SocketAddrV4::new(address, port));
        held.push(socket.map_err(|_| refused(TAKEN))?);
    }
    drop(held);
    Ok(())
}

/// A new socket of `transport` bound to `address`, without address reuse:
/// a port that any other socket holds there is refused, as the relay
/// itself would be.
pub(super) fn bound(transport: Transport, address: SocketAddrV4) -> std::io::Result<OwnedFd> {
    let kind = match transport {
        Transport::Udp => SocketType::DGRAM,
        Transport::Tcp => SocketType::STREAM,
    };
    let socket = rustix::net::socket(AddressFamily::INET, kind, None)?;
    rustix::net::bind(&socket, &address)?;
    Ok(socket)
}

/// What preparing the relay's shared secret did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Prepared {
    /// There was nothing at the path and a new secret is there now.
    Created,
    /// The path already held a private secret, which is kept unchanged.
    Retained,
}

impl Prepared {
    /// The word the playbook reads to report a change.
    pub(super) fn word(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Retained => "retained",
        }
    }
}

/// Keep the secret at `path` when it is a regular file of `owner`, mode
/// 0600, holding exactly 64 lowercase hexadecimal digits; write a new one
/// there when the path is absent. Anything else at the path refuses, and an
/// existing name is never replaced: a link, even a dangling one, is
/// refused. The new secret is 32 bytes of the system's random source in
/// hexadecimal, created exclusively with mode 0600 and synced; a write that
/// fails removes the file it created. Blocks on file I/O only.
pub(super) fn prepare(path: &Path, owner: u32) -> Result<Prepared> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => retain(path, &metadata, owner).map(|()| Prepared::Retained),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            create(path).map(|()| Prepared::Created)
        }
        Err(error) => Err(Fault::from(error)),
    }
}

/// Require the secret that is already at `path`, read through a
/// descriptor that follows no link and is the file `metadata` described.
fn retain(path: &Path, metadata: &std::fs::Metadata, owner: u32) -> Result<()> {
    require(
        metadata.file_type().is_file() && metadata.uid() == owner,
        UNSAFE,
    )?;
    require(
        metadata.mode() & 0o7777 == 0o600 && metadata.len() == DIGITS as u64,
        UNSAFE,
    )?;
    let flags = OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(flags.bits())
        .open(path)
        .map_err(|_| refused(UNSAFE))?;
    let opened = file.metadata()?;
    require(
        opened.dev() == metadata.dev() && opened.ino() == metadata.ino(),
        UNSAFE,
    )?;
    let mut text = Vec::with_capacity(DIGITS + 1);
    file.take(DIGITS as u64 + 1).read_to_end(&mut text)?;
    let digits = std::str::from_utf8(&text).is_ok_and(|text| hexadecimal(text, DIGITS));
    require(digits, UNSAFE)
}

/// Write a new secret at `path`, which must not exist.
fn create(path: &Path) -> Result<()> {
    let mut random = [0_u8; DIGITS / 2];
    getrandom::fill(&mut random).map_err(|_| Fault::Kind("OSError"))?;
    let text: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    let written = file
        .write_all(text.as_bytes())
        .and_then(|()| file.sync_all());
    if let Err(error) = written {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(Fault::from(error));
    }
    Ok(())
}

fn refused(text: &str) -> Fault {
    Fault::Refused(text.to_owned())
}

/// The line a failed preparation step leaves the caller: a refusal in its
/// own fixed words, anything else by its kind.
pub(super) fn said(fault: &Fault) -> String {
    match fault {
        Fault::Refused(text) => format!("{text}\n"),
        Fault::Kind(kind) => format!("Relay preparation failed ({kind})\n"),
    }
}
