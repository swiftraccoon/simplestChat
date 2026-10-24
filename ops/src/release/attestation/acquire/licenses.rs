//! Exact scoped httparse notice alongside the unchanged cached ZIP notice.
use std::{
    io::{self, Write},
    os::fd::{AsFd, BorrowedFd},
};
pub(super) const LICENSE: &str = concat!(
    "Copyright (c) 2015-2025 Sean McArthur\n",
    "\n",
    "Permission is hereby granted, free of charge, to any person obtaining a copy",
    "\n",
    "of this software and associated documentation files (the \"Software\"), to dea",
    "l\n",
    "in the Software without restriction, including without limitation the rights",
    "\n",
    "to use, copy, modify, merge, publish, distribute, sublicense, and/or sell\n",
    "copies of the Software, and to permit persons to whom the Software is\n",
    "furnished to do so, subject to the following conditions:\n",
    "\n",
    "The above copyright notice and this permission notice shall be included in\n",
    "all copies or substantial portions of the Software.\n",
    "\n",
    "THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR\n",
    "IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,\n",
    "FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE\n",
    "AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER\n",
    "LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM",
    ",\n",
    "OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN\n",
    "THE SOFTWARE.\n",
    "\n",
);
pub(super) const RANDOM_LICENSE: &str = concat!(
    "Copyright (c) 2018-2026 The rust-random Project Developers\n",
    "Copyright (c) 2014 The Rust Project Developers\n",
    "\n",
    "Permission is hereby granted, free of charge, to any\n",
    "person obtaining a copy of this software and associated\n",
    "documentation files (the \"Software\"), to deal in the\n",
    "Software without restriction, including without\n",
    "limitation the rights to use, copy, modify, merge,\n",
    "publish, distribute, sublicense, and/or sell copies of\n",
    "the Software, and to permit persons to whom the Software\n",
    "is furnished to do so, subject to the following\n",
    "conditions:\n",
    "\n",
    "The above copyright notice and this permission notice\n",
    "shall be included in all copies or substantial portions\n",
    "of the Software.\n",
    "\n",
    "THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF\n",
    "ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED\n",
    "TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A\n",
    "PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT\n",
    "SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY\n",
    "CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION\n",
    "OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR\n",
    "IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER\n",
    "DEALINGS IN THE SOFTWARE.\n",
);
pub(super) fn write(mut output: impl Write) -> io::Result<()> {
    output.write_all(
        "Fresh-release dependency notices (limited scope)\n\nhttparse 1.10.1 — MIT\n\n".as_bytes(),
    )?;
    output.write_all(LICENSE.as_bytes())?;
    output.write_all(b"\ngetrandom 0.4.3 - MIT\n\n")?;
    output.write_all(RANDOM_LICENSE.as_bytes())?;
    output.write_all(b"\n")?;
    super::super::verify::licenses::write(output)
}
pub(super) struct Descriptor<'a>(pub(super) BorrowedFd<'a>);
impl Write for Descriptor<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        rustix::io::write(self.0, bytes).map_err(io::Error::from)
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(in crate::release::attestation) fn stdout() -> io::Result<()> {
    let output = io::stdout();
    let mut lock = output.lock();
    lock.flush()?;
    write(Descriptor(lock.as_fd()))
}
