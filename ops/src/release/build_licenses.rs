//! Exact MIT notices for the release builder's explicit Tokio/runtime use.
//! This limited set is not a complete Ops/workspace dependency-license inventory.
//! Texts come from the Cargo.lock-authenticated crates; tests bind their bytes.

use std::{
    io::{self, Write},
    os::fd::{AsFd, BorrowedFd},
};

const HEADER: &str = "Release-builder dependency notices (limited scope)\n\n\
These notices cover Tokio 1.53.3, Mio 1.2.4 and signal-hook-registry 1.4.8.\n\
The MIT option is selected for signal-hook-registry.\n\
This is a limited release-builder notice set; the full dependency audit is separate.\n";

const TOKIO: &str = r#"MIT License

Copyright (c) Tokio Contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
"#;

const MIO: &str = r#"Copyright (c) 2014 Carl Lerche and other MIO contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in
all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN
THE SOFTWARE.
"#;

const SIGNAL_HOOK_REGISTRY: &str = r#"Copyright (c) 2017 tokio-jsonrpc developers

Permission is hereby granted, free of charge, to any
person obtaining a copy of this software and associated
documentation files (the "Software"), to deal in the
Software without restriction, including without
limitation the rights to use, copy, modify, merge,
publish, distribute, sublicense, and/or sell copies of
the Software, and to permit persons to whom the Software
is furnished to do so, subject to the following
conditions:

The above copyright notice and this permission notice
shall be included in all copies or substantial portions
of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF
ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED
TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A
PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT
SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION
OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR
IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
DEALINGS IN THE SOFTWARE.
"#;

// StdoutLock deliberately converts EBADF into success. Keep its lock and
// flush its buffer first, then borrow its descriptor for checked kernel writes.
// No descriptor is duplicated, adopted, reconfigured or closed here.
struct Descriptor<'a>(BorrowedFd<'a>);
impl Write for Descriptor<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        rustix::io::write(self.0, bytes).map_err(io::Error::from)
    }
    fn flush(&mut self) -> io::Result<()> {
        // This adapter has no user-space buffer.
        Ok(())
    }
}

pub(super) fn stdout() -> io::Result<()> {
    let output = io::stdout();
    let mut locked = output.lock();
    locked.flush()?;
    write(Descriptor(locked.as_fd()))
}

fn write(mut output: impl Write) -> io::Result<()> {
    output.write_all(HEADER.as_bytes())?;
    for (label, notice) in [
        ("Tokio 1.53.3 (MIT)", TOKIO),
        ("Mio 1.2.4 (MIT)", MIO),
        (
            "signal-hook-registry 1.4.8 (MIT option)",
            SIGNAL_HOOK_REGISTRY,
        ),
    ] {
        writeln!(output, "\n--- {label} ---")?;
        output.write_all(notice.as_bytes())?;
    }
    output.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn notices_match_the_three_authenticated_locked_crate_members() {
        for (text, bytes, expected) in [
            (
                TOKIO,
                1070,
                "253cd04c6714889df2d32f3f64d669179a1c95c76ac43c40882c52eb06bc3552",
            ),
            (
                MIO,
                1082,
                "07919255c7e04793d8ea760d6c2ce32d19f9ff02bdbdde3ce90b1e1880929a9b",
            ),
            (
                SIGNAL_HOOK_REGISTRY,
                1068,
                "503558bfefe66ca15e4e3f7955b3cb0ec87fd52f29bf24b336af7bd00e946d5c",
            ),
        ] {
            assert_eq!(text.len(), bytes);
            let digest: String = Sha256::digest(text.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            assert_eq!(digest, expected);
        }
    }

    #[test]
    fn borrowed_read_only_descriptor_preserves_the_actual_write_error() {
        let input = std::fs::File::open("/dev/null").unwrap();
        let result = write(Descriptor(input.as_fd()));
        let closed = nix::unistd::close(input);
        assert_eq!(
            result.unwrap_err().raw_os_error(),
            Some(rustix::io::Errno::BADF.raw_os_error())
        );
        closed.unwrap();
    }

    #[test]
    fn partial_write_and_late_flush_errors_are_returned() {
        struct RefusesWrite(usize);
        impl Write for RefusesWrite {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                if self.0 == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "PRIVATE writer error",
                    ));
                }
                let count = self.0.min(bytes.len());
                self.0 -= count;
                Ok(count)
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            write(RefusesWrite(13)).unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );

        struct RefusesFlush(Vec<u8>);
        impl Write for RefusesFlush {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Err(io::Error::other("PRIVATE flush error"))
            }
        }
        let mut expected = Vec::new();
        write(&mut expected).unwrap();
        let mut output = RefusesFlush(Vec::new());
        assert!(write(&mut output).is_err());
        assert_eq!(output.0, expected);
    }
}
