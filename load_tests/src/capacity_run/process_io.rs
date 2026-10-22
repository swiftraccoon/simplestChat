//! Concurrent stream sinks apply byte ceilings before retaining any chunk.

use anyhow::{Result, bail};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

pub(super) enum Destination {
    Capture(Vec<u8>),
    File(tokio::fs::File),
}

impl Destination {
    pub(super) fn capture() -> Self {
        Self::Capture(Vec::new())
    }
    pub(super) fn file(file: std::fs::File) -> Self {
        Self::File(tokio::fs::File::from_std(file))
    }
}

pub(super) async fn read_limited(
    mut reader: impl AsyncRead + Unpin,
    mut destination: Destination,
    limit: u64,
    name: &str,
) -> Result<Vec<u8>> {
    let mut retained = 0;
    let mut buffer = vec![0_u8; 65536];
    loop {
        let size = reader.read(&mut buffer).await?;
        if size == 0 {
            break;
        }
        let available = (limit - retained).min(size as u64) as usize;
        match &mut destination {
            Destination::Capture(bytes) => bytes.extend_from_slice(&buffer[..available]),
            Destination::File(file) => file.write_all(&buffer[..available]).await?,
        }
        retained += available as u64;
        if available < size {
            if let Destination::File(file) = &mut destination {
                file.flush().await?;
            }
            bail!("command_{name}_limit_exceeded");
        }
    }
    match destination {
        Destination::Capture(bytes) => Ok(bytes),
        Destination::File(mut file) => {
            file.flush().await?;
            // Await every queued filesystem operation, then report the exact
            // descriptor's close before a successful transfer can be published.
            nix::unistd::close(file.into_std().await)?;
            Ok(Vec::new())
        }
    }
}
