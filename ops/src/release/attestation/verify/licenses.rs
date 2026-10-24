//! The exact authenticated MIT notice for this operation's new rawzip edge.
//! This limited notice is not a complete Ops/workspace license inventory.

use std::{
    io::{self, Write},
    os::fd::{AsFd, BorrowedFd},
};
const HEADER: &str = concat!(
    "Cached-verifier dependency notice (limited scope)\n\n",
    "rawzip 0.5.2 — MIT\n\n"
);
const LICENSE: &str = concat!(
    "Permission is hereby granted, free of charge, to any person obtaining a copy of\n",
    r#"this software and associated documentation files (the "Software"), to deal in
the Software without restriction, including without limitation the rights to
use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies
of the Software, and to permit persons to whom the Software is furnished to do
so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE."#
);
pub(in crate::release::attestation) fn write(mut writer: impl Write) -> io::Result<()> {
    writer.write_all(HEADER.as_bytes())?;
    writer.write_all(LICENSE.as_bytes())?;
    writer.write_all(
        concat!(
            "\n\nRuntime notices: release build --licenses. ",
            "This is not a complete license inventory.\n"
        )
        .as_bytes(),
    )?;
    writer.flush()
}
struct Descriptor<'a>(BorrowedFd<'a>);
impl Write for Descriptor<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        rustix::io::write(self.0, bytes).map_err(io::Error::from)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(super) fn stdout() -> io::Result<()> {
    let output = io::stdout();
    let mut lock = output.lock();
    lock.flush()?;
    write(Descriptor(lock.as_fd()))
}
#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};
    #[test]
    fn original_mit_notice_bytes_are_embedded_without_reencoding() {
        assert_eq!(LICENSE.len(), 1022);
        let hash: String = Sha256::digest(LICENSE.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(
            hash,
            "2af4d13698993a15849a869b4c2ed4954cf251768ad145f371774adb96697e0c"
        );
    }
    #[test]
    fn partial_write_flush_and_readonly_descriptor_failures_remain_negative() {
        struct Fault {
            left: usize,
            flush: bool,
        }
        impl Write for Fault {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.left == 0 {
                    return Err(io::Error::other("PRIVATE write"));
                }
                let size = self.left.min(bytes.len());
                self.left -= size;
                Ok(size)
            }
            fn flush(&mut self) -> io::Result<()> {
                if self.flush {
                    Err(io::Error::other("PRIVATE flush"))
                } else {
                    Ok(())
                }
            }
        }
        assert!(
            write(Fault {
                left: 3,
                flush: false
            })
            .is_err()
        );
        assert!(
            write(Fault {
                left: usize::MAX,
                flush: true
            })
            .is_err()
        );
        let input = std::fs::File::open("/dev/null").unwrap();
        let refused = write(Descriptor(input.as_fd()));
        let closed = nix::unistd::close(input);
        assert_eq!(
            refused.unwrap_err().raw_os_error(),
            Some(rustix::io::Errno::BADF.raw_os_error())
        );
        closed.unwrap();
    }
}
