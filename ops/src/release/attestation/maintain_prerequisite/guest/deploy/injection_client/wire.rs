//! Fixed framed response and durable-receipt exchange under the inherited end.
use super::context::{Context, Fence, stamp};
use super::*;
use rustix::net::{AddressFamily, SocketAddrUnix, SocketFlags, SocketType};
use std::{
    fs,
    io::{Read, Write},
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        net::UnixStream,
    },
};

pub(super) fn read(peer: &mut UnixStream, count: usize, fence: &Fence) -> Result<Vec<u8>> {
    require(count <= STREAM)?;
    let mut raw = vec![0; count];
    let mut at = 0;
    while at < count {
        fence.check()?;
        peer.set_read_timeout(Some(fence.remaining()?))
            .map_err(|_| Error::Io)?;
        match peer.read(&mut raw[at..(at + CHUNK).min(count)]) {
            Ok(0) => return Err(Error::Refused),
            Ok(count) => at += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(Error::Io),
        }
        fence.check()?;
    }
    fence.check()?;
    Ok(raw)
}
pub(super) fn write(peer: &mut UnixStream, raw: &[u8], fence: &Fence) -> Result<()> {
    require(raw.len() <= STREAM)?;
    let mut at = 0;
    while at < raw.len() {
        fence.check()?;
        peer.set_write_timeout(Some(fence.remaining()?))
            .map_err(|_| Error::Io)?;
        match peer.write(&raw[at..(at + CHUNK).min(raw.len())]) {
            Ok(0) => return Err(Error::Refused),
            Ok(count) => at += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(Error::Io),
        }
        fence.check()?;
    }
    fence.check()
}
pub(super) fn receive(peer: &mut UnixStream, fence: &Fence) -> Result<Value> {
    let size = u32::from_be_bytes(
        read(peer, 4, fence)?
            .try_into()
            .map_err(|_| Error::Refused)?,
    ) as usize;
    require(size > 0 && size <= FRAME)?;
    let raw = read(peer, size, fence)?;
    require(!raw.starts_with(&[0xef, 0xbb, 0xbf]))?;
    let value = decode(&raw)?;
    require(value.is_object())?;
    fence.check()?;
    Ok(value)
}
pub(super) fn send(peer: &mut UnixStream, value: &Value, fence: &Fence) -> Result<()> {
    let raw = serde_json::to_vec(value).map_err(|_| Error::Refused)?;
    require(!raw.is_empty() && raw.len() <= FRAME)?;
    write(peer, &(raw.len() as u32).to_be_bytes(), fence)?;
    write(peer, &raw, fence)
}
fn connect(peer: &UnixStream, fence: &Fence) -> Result<()> {
    let address = SocketAddrUnix::new(SOCKET).map_err(|_| Error::Refused)?;
    loop {
        fence.check()?;
        match rustix::net::connect(peer, &address) {
            Ok(()) | Err(rustix::io::Errno::ISCONN) => break,
            Err(
                rustix::io::Errno::INPROGRESS
                | rustix::io::Errno::ALREADY
                | rustix::io::Errno::AGAIN
                | rustix::io::Errno::INTR,
            ) => {}
            Err(_) => return Err(Error::Io),
        }
        let left = fence.remaining()?.min(std::time::Duration::from_millis(20));
        let mut events = [rustix::event::PollFd::new(
            peer,
            rustix::event::PollFlags::OUT,
        )];
        match rustix::event::poll(
            &mut events,
            Some(&rustix::event::Timespec {
                tv_sec: 0,
                tv_nsec: i64::from(left.subsec_nanos()),
            }),
        ) {
            Ok(_) | Err(rustix::io::Errno::INTR) => {}
            Err(_) => return Err(Error::Io),
        }
        if let Ok(address) = peer.peer_addr() {
            require(address.as_pathname() == Some(Path::new(SOCKET)))?;
            break;
        }
    }
    peer.set_nonblocking(false).map_err(|_| Error::Io)?;
    fence.check()
}
fn receipt(
    context: &mut Context,
    response: &Value,
    id: &str,
    out: &[u8],
    err: &[u8],
) -> Result<String> {
    let expected = digest(&response["receiptSha256"])?;
    let original = context.open(RECEIPT, FRAME as u64, 0o600)?;
    let raw = context.original(original)?.data(context.fence()?)?;
    let config = context.config()?;
    receipt_value(&raw, expected, &config.nonce, &config.image, id, out, err)?;
    context.original(original)?.check()?;
    Ok(expected.into())
}
pub(super) fn receipt_value(
    raw: &[u8],
    expected: &str,
    nonce: &str,
    image: &str,
    id: &str,
    out: &[u8],
    err: &[u8],
) -> Result<()> {
    require(raw.len() <= FRAME && out.len() <= STREAM && err.len() <= STREAM)?;
    require(hash(raw) == expected)?;
    let value = decode(raw)?;
    exact(
        &value,
        &[
            "schemaVersion",
            "scope",
            "nonce",
            "identifier",
            "image",
            "responseRawExit",
            "stdoutBytes",
            "stdoutSha256",
            "stderrBytes",
            "stderrSha256",
            "runningBefore",
            "runningAfter",
            "stopRawExit",
            "finishedChanged",
        ],
    )?;
    require(
        value["schemaVersion"].as_u64() == Some(1)
            && value["scope"] == "HostDeployInspectionBarrier"
            && value["nonce"] == nonce
            && value["identifier"] == id
            && value["image"] == image
            && value["responseRawExit"].as_i64() == Some(0)
            && value["stopRawExit"].as_i64() == Some(0)
            && value["runningBefore"] == true
            && value["runningAfter"] == false
            && value["finishedChanged"] == true
            && value["stdoutBytes"].as_u64() == Some(out.len() as u64)
            && value["stderrBytes"].as_u64() == Some(err.len() as u64)
            && digest(&value["stdoutSha256"])? == hash(out)
            && digest(&value["stderrSha256"])? == hash(err),
    )?;
    Ok(())
}
fn output(writer: &mut impl Write, raw: &[u8], fence: &Fence) -> Result<()> {
    for bytes in raw.chunks(CHUNK) {
        fence.check()?;
        writer.write_all(bytes).map_err(|_| Error::Io)?;
        fence.check()?;
    }
    writer.flush().map_err(|_| Error::Io)?;
    fence.check()
}
pub(super) fn completed(
    peer: &mut UnixStream,
    nonce: &str,
    receipt: Option<&str>,
    fence: &Fence,
) -> Result<()> {
    send(
        peer,
        &json!({"action":"received","nonce":nonce,"receiptSha256":receipt}),
        fence,
    )?;
    let observed = receive(peer, fence)?;
    require(
        observed
            == json!({"action":"complete","nonce":nonce,
        "receiptSha256":receipt}),
    )?;
    peer.set_read_timeout(Some(fence.remaining()?))
        .map_err(|_| Error::Io)?;
    let mut extra = [0; 1];
    loop {
        fence.check()?;
        match peer.read(&mut extra) {
            Ok(0) => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            _ => return Err(Error::Refused),
        }
    }
    fence.check()
}
pub(super) fn client(context: &mut Context, id: &str) -> Result<bool> {
    let original = context.real()?;
    context.check()?;
    let before = fs::symlink_metadata(SOCKET).map_err(|_| Error::Io)?;
    require(
        before.file_type().is_socket()
            && before.uid() == 0
            && before.gid() == 0
            && before.mode() & 0o7777 == 0o600,
    )?;
    let descriptor = rustix::net::socket_with(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::NONBLOCK | SocketFlags::CLOEXEC,
        None,
    )
    .map_err(|_| Error::Io)?;
    let mut peer = UnixStream::from(descriptor);
    let body = (|| {
        connect(&peer, context.fence()?)?;
        context.check()?;
        require(stamp(&before)? == stamp(&fs::symlink_metadata(SOCKET).map_err(|_| Error::Io)?)?)?;
        let credentials = rustix::net::sockopt::socket_peercred(&peer).map_err(|_| Error::Io)?;
        require(credentials.uid.as_raw() == 0 && credentials.gid.as_raw() == 0)?;
        let pid = credentials.pid.as_raw_nonzero().get();
        require(
            fs::metadata(format!("/proc/{pid}/ns/mnt"))
                .map_err(|_| Error::Io)?
                .ino()
                == context.config()?.namespace,
        )?;
        send(
            &mut peer,
            &json!({"schemaVersion":1,"nonce":context.config()?.nonce,
            "identifier":id}),
            context.fence()?,
        )?;
        let response = receive(&mut peer, context.fence()?)?;
        context.check()?;
        let (deliver, receipt) = if response["action"] == "pass" {
            exact(&response, &["action"])?;
            (false, None)
        } else {
            exact(
                &response,
                &["action", "stdoutBytes", "stderrBytes", "receiptSha256"],
            )?;
            require(response["action"] == "deliver")?;
            let count = |key: &str| {
                response[key]
                    .as_u64()
                    .filter(|size| *size <= STREAM as u64)
                    .and_then(|size| usize::try_from(size).ok())
                    .ok_or(Error::Bounds)
            };
            let out = read(&mut peer, count("stdoutBytes")?, context.fence()?)?;
            let err = read(&mut peer, count("stderrBytes")?, context.fence()?)?;
            let receipt = receipt(context, &response, id, &out, &err)?;
            context.check()?;
            output(&mut std::io::stdout().lock(), &out, context.fence()?)?;
            output(&mut std::io::stderr().lock(), &err, context.fence()?)?;
            (true, Some(receipt))
        };
        completed(
            &mut peer,
            &context.config()?.nonce,
            receipt.as_deref(),
            context.fence()?,
        )?;
        context.check()?;
        context.original(original)?.check()?;
        Ok(deliver)
    })();
    let closed = nix::unistd::close(peer).map_err(|_| Error::Close);
    body.and_then(|result| closed.map(|_| result))
}
