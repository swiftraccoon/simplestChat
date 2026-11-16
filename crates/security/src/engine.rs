//! The container engine a gate runs its scanners in: one on this host, named
//! by the caller, never one that an ambient context points elsewhere.

use crate::{Fault, Result, document::Document, require};
use std::{
    ffi::OsString,
    net::Ipv6Addr,
    path::{Path, PathBuf},
};

/// A list of the engine's connections that is no list of records, or a
/// connection whose address is no text or no address.
const UNREAD: Fault = Fault("podman_connections_unread");
/// The connection of the Podman machine on a Mac.
const MACHINE: &str = "podman-machine-default";

/// The engines a gate may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Name {
    /// Docker, through the socket of this host.
    Docker,
    /// Podman, on this host or in its machine on a Mac.
    Podman,
}

impl Name {
    /// The engine named `text`, if it is one of them.
    pub fn parsed(text: &str) -> Option<Self> {
        match text {
            "docker" => Some(Self::Docker),
            "podman" => Some(Self::Podman),
            _ => None,
        }
    }

    /// The name of its program.
    pub const fn program(self) -> &'static str {
        match self {
            Self::Docker => "docker",
            Self::Podman => "podman",
        }
    }
}

/// What the selection of an engine looks at on this host.
pub struct Host<'a> {
    /// Where the program of a name is on the caller's search path.
    pub located: &'a dyn Fn(&'static str) -> Option<PathBuf>,
    /// The caller's home.
    pub home: Option<PathBuf>,
    /// Whether something is at a path, a final link followed.
    pub present: &'a dyn Fn(&Path) -> bool,
    /// Whether this is a Mac, where Podman runs in a machine of its own.
    pub mac: bool,
    /// What a command printed; refused unless it ended with 0.
    pub asked: &'a mut dyn FnMut(&[OsString]) -> Result<Vec<u8>>,
}

/// The command line that reaches the local engine `name` and no other:
/// Docker through the first socket of this host that is there, Podman
/// without a remote connection, or on a Mac through its machine, once the
/// machine's connection is proved to end on this host.
pub fn prefix(name: Name, host: &mut Host<'_>) -> Result<Vec<OsString>> {
    let program = (host.located)(name.program()).ok_or(Fault("engine_missing"))?;
    match name {
        Name::Docker => {
            let mut sockets = vec![PathBuf::from("/var/run/docker.sock")];
            sockets.extend(
                host.home
                    .iter()
                    .map(|home| home.join(".docker/run/docker.sock")),
            );
            let socket = sockets.into_iter().find(|path| (host.present)(path));
            let socket = socket.ok_or(Fault("local_docker_socket_missing"))?;
            let mut address = OsString::from("unix://");
            address.push(socket);
            Ok(vec![program.into(), "--host".into(), address])
        }
        Name::Podman if !host.mac => Ok(vec![program.into(), "--remote=false".into()]),
        Name::Podman => {
            let mut listing: Vec<OsString> = vec![program.clone().into()];
            listing
                .extend(["system", "connection", "list", "--format", "json"].map(OsString::from));
            let listed = Document::read(&(host.asked)(&listing)?).map_err(|_| UNREAD)?;
            let mut machines = Vec::new();
            for connection in listed.items().ok_or(UNREAD)? {
                require(connection.fields().is_some(), UNREAD.0)?;
                if connection.get("Name").and_then(Document::text) == Some(MACHINE) {
                    machines.push(connection);
                }
            }
            let [machine] = machines[..] else {
                return Err(Fault("local_podman_machine_missing"));
            };
            let address = machine.get("URI").and_then(Document::text).ok_or(UNREAD)?;
            let (scheme, reached) = split(address)?;
            let local = ["127.0.0.1", "localhost", "::1"].contains(&reached.as_str());
            require(scheme == "ssh" && local, "podman_machine_not_local")?;
            Ok(vec![program.into(), "--connection".into(), MACHINE.into()])
        }
    }
}

/// The scheme of the address `text` and the host it names in small
/// letters, as the reference splits an address: what precedes it up to the
/// first character that is no control or space is dropped, so are tabs and
/// line ends anywhere, and a bracketed host must be an IPv6 address or a
/// future one. An address that names no host names an empty one, which is
/// no more local than a host with a zone, whatever its letters.
fn split(text: &str) -> Result<(String, String)> {
    let text = text.trim_start_matches(|character: char| character <= ' ');
    let text: String = text
        .chars()
        .filter(|character| !"\t\r\n".contains(*character))
        .collect();
    let mut scheme = String::new();
    let mut rest = text.as_str();
    if let Some(colon) = text.find(':')
        && text.starts_with(|first: char| first.is_ascii_alphabetic())
        && text[..colon]
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "+-.".contains(character))
    {
        scheme = text[..colon].to_ascii_lowercase();
        rest = &text[colon + 1..];
    }
    let Some(after) = rest.strip_prefix("//") else {
        return Ok((scheme, String::new()));
    };
    let place = after.find(['/', '?', '#']).unwrap_or(after.len());
    let place = &after[..place];
    require(place.contains('[') == place.contains(']'), UNREAD.0)?;
    let named = place.rsplit_once('@').map_or(place, |(_, named)| named);
    if place.contains('[') {
        // A bracket anywhere holds the host to what may stand between two.
        let checked = match named.split_once('[') {
            Some((before, bracketed)) => {
                let (host, port) = bracketed.split_once(']').unwrap_or((bracketed, ""));
                require(
                    before.is_empty() && (port.is_empty() || port.starts_with(':')),
                    UNREAD.0,
                )?;
                host
            }
            None => unbracketed(named),
        };
        require(bracketed_host(checked), UNREAD.0)?;
    }
    let host = match named.split_once('[') {
        Some((_, bracketed)) => bracketed
            .split_once(']')
            .map_or(bracketed, |(host, _)| host),
        None => unbracketed(named),
    };
    Ok((scheme, host.to_lowercase()))
}

/// The host of `named`, which holds no bracket: what precedes its port.
fn unbracketed(named: &str) -> &str {
    named.split_once(':').map_or(named, |(host, _)| host)
}

/// Whether `host`, written between brackets, is an IPv6 address, scoped or
/// not, or an address of a future version.
fn bracketed_host(host: &str) -> bool {
    if let Some(future) = host.strip_prefix(['v', 'V']) {
        let Some((version, rest)) = future.split_once('.') else {
            return false;
        };
        return !version.is_empty()
            && version.chars().all(|digit| digit.is_ascii_hexdigit())
            && !rest.is_empty();
    }
    let (address, zone) = host
        .split_once('%')
        .map_or((host, None), |(address, zone)| (address, Some(zone)));
    let scoped = zone.is_none_or(|zone| !zone.is_empty() && !zone.contains('%'));
    scoped && address.parse::<Ipv6Addr>().is_ok()
}

#[cfg(test)]
#[path = "engine_tests.rs"]
mod tests;
