//! Metadata-only bytes shared by unit/CLI tests. They contain no program code.

pub const BASE: u64 = 0x400000;
pub const DYNAMIC: usize = 512;
pub const STRINGS: usize = 1024;
pub const INTERPRETER: usize = 1280;

pub fn u16_at(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
pub fn u32_at(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
pub fn u64_at(bytes: &mut [u8], offset: usize, value: u64) {
    bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}
pub fn program(bytes: &mut [u8], index: usize, kind: u32, flags: u32, offset: usize, size: usize) {
    let start = 64 + index * 56;
    u32_at(bytes, start, kind);
    u32_at(bytes, start + 4, flags);
    u64_at(bytes, start + 8, offset as u64);
    u64_at(bytes, start + 16, BASE + offset as u64);
    u64_at(bytes, start + 32, size as u64);
    u64_at(bytes, start + 40, size as u64);
    u64_at(bytes, start + 48, 1);
}
pub fn dynamic(bytes: &mut [u8], entries: &[(u64, u64)]) {
    for (index, (tag, value)) in entries.iter().enumerate() {
        u64_at(bytes, DYNAMIC + index * 16, *tag);
        u64_at(bytes, DYNAMIC + index * 16 + 8, *value);
    }
    program(bytes, 2, 2, 4, DYNAMIC, entries.len() * 16);
}
pub fn fixture(dynamic_table: bool, interpreter: bool, needed: bool) -> Vec<u8> {
    let mut bytes = vec![0; 4096];
    bytes[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
    u16_at(&mut bytes, 16, if dynamic_table { 3 } else { 2 });
    u16_at(&mut bytes, 18, 62);
    u32_at(&mut bytes, 20, 1);
    u64_at(&mut bytes, 24, BASE + 64);
    u64_at(&mut bytes, 32, 64);
    u16_at(&mut bytes, 52, 64);
    u16_at(&mut bytes, 54, 56);
    u16_at(&mut bytes, 56, 5);
    program(&mut bytes, 0, 1, 5, 0, 4096);
    program(&mut bytes, 1, 0x6474e551, 6, 0, 0);
    if dynamic_table {
        let strings = b"\0PRIVATE-SENTINEL-loader.so\0";
        bytes[STRINGS..STRINGS + strings.len()].copy_from_slice(strings);
        let mut entries = vec![
            (5, BASE + STRINGS as u64),
            (10, strings.len() as u64),
            (0x6ffffffb, 0x08000001),
        ];
        if needed {
            entries.push((1, 1));
        }
        entries.push((0, 0));
        dynamic(&mut bytes, &entries);
    }
    if interpreter {
        let value = b"/PRIVATE-SENTINEL/ld.so\0";
        bytes[INTERPRETER..INTERPRETER + value.len()].copy_from_slice(value);
        program(&mut bytes, 3, 3, 4, INTERPRETER, value.len());
    }
    program(&mut bytes, 4, 0x6474e552, 4, DYNAMIC, 64);
    bytes
}
