//! A directory of one test's own, removed with it.

use std::path::PathBuf;

pub(crate) struct Scratch(pub(crate) PathBuf);

impl Scratch {
    pub(crate) fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "simplestchat-security-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("scratch directory");
        Self(path.canonicalize().expect("scratch path"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Standard base64, as a recording holds bytes that are no text.
pub(crate) fn base64(text: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bytes = Vec::new();
    let (mut bits, mut held) = (0_u32, 0);
    for symbol in text.bytes().filter(|symbol| *symbol != b'=') {
        let value = ALPHABET.iter().position(|letter| *letter == symbol);
        bits = bits << 6 | u32::try_from(value.expect("a base64 symbol")).expect("six bits");
        held += 6;
        if held >= 8 {
            held -= 8;
            bytes.push(u8::try_from(bits >> held & 0xff).expect("a byte"));
        }
    }
    bytes
}
