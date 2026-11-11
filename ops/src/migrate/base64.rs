//! Standard base64 with padding, as the envelope's files are spelled. The
//! decoder admits nothing else: no line breaks, no missing or stray padding.

use crate::host::fault::{Fault, Result};

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
/// Text that is not base64.
const MALFORMED: Fault = Fault::Kind("Error");

pub(super) fn encode(data: &[u8]) -> String {
    let mut text = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let bits = chunk
            .iter()
            .fold(0_u32, |bits, byte| bits << 8 | u32::from(*byte))
            << (8 * (3 - chunk.len()));
        for place in 0..4 {
            let sextet = (bits >> (18 - 6 * place)) & 0x3f;
            let letter = ALPHABET[usize::try_from(sextet).unwrap_or_default()];
            text.push(if place <= chunk.len() {
                char::from(letter)
            } else {
                '='
            });
        }
    }
    text
}

pub(super) fn decode(text: &str) -> Result<Vec<u8>> {
    let bytes = text.as_bytes();
    if !bytes.len().is_multiple_of(4) {
        return Err(MALFORMED);
    }
    let padding = bytes.iter().rev().take_while(|byte| **byte == b'=').count();
    if padding > 2 {
        return Err(MALFORMED);
    }
    let mut data = Vec::with_capacity(bytes.len() / 4 * 3);
    let digits = &bytes[..bytes.len() - padding];
    for quad in digits.chunks(4) {
        let mut bits = 0_u32;
        for letter in quad {
            let sextet = ALPHABET
                .iter()
                .position(|known| known == letter)
                .ok_or(MALFORMED)?;
            bits = bits << 6 | u32::try_from(sextet).map_err(|_| MALFORMED)?;
        }
        bits <<= 6 * (4 - quad.len());
        let whole = bits.to_be_bytes();
        data.extend_from_slice(&whole[1..quad.len()]);
    }
    Ok(data)
}
