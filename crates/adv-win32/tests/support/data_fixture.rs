//! An authored PE32 EXE with data imports. No external executable is used.
#![allow(dead_code)]
pub const BASE: u32 = 0x400000;
pub const ENTRY: u32 = BASE + 0x1000;
pub const IAT: u32 = BASE + 0x2060;
pub const OUTPUT: u32 = BASE + 0x2300;
pub fn name(index: u32) -> u32 {
    BASE + 0x2102 + index * 32
}
pub fn word(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
pub fn image(module: &str, names: &[&str]) -> Vec<u8> {
    assert!(names.len() <= 3);
    let mut bytes = vec![0; 0x3000];
    bytes[..2].copy_from_slice(b"MZ");
    word(&mut bytes, 0x3c, 0x80);
    bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
    bytes[0x84..0x86].copy_from_slice(&0x14cu16.to_le_bytes());
    bytes[0x86..0x88].copy_from_slice(&2u16.to_le_bytes());
    bytes[0x94..0x96].copy_from_slice(&224u16.to_le_bytes());
    bytes[0x96..0x98].copy_from_slice(&0x102u16.to_le_bytes());
    bytes[0x98..0x9a].copy_from_slice(&0x10bu16.to_le_bytes());
    for (offset, value) in [
        (16, 0x1000),
        (20, 0x1000),
        (28, BASE),
        (32, 4096),
        (36, 4096),
        (56, 0x3000),
        (60, 4096),
        (72, 65536),
        (76, 4096),
        (92, 16),
        (104, 0x2000),
        (108, 40),
    ] {
        word(&mut bytes, 0x98 + offset, value);
    }
    bytes[0x98 + 68..0x98 + 70].copy_from_slice(&3u16.to_le_bytes());
    for (index, (name, rva, flags)) in [
        (b".text".as_slice(), 0x1000, 0x60000020),
        (b".idata".as_slice(), 0x2000, 0xc0000040),
    ]
    .into_iter()
    .enumerate()
    {
        let at = 0x178 + index * 40;
        bytes[at..at + name.len()].copy_from_slice(name);
        for (offset, value) in [(8, 4096), (12, rva), (16, 4096), (20, rva), (36, flags)] {
            word(&mut bytes, at + offset, value);
        }
    }
    word(&mut bytes, 0x2000, 0x2040);
    word(&mut bytes, 0x200c, 0x2080);
    word(&mut bytes, 0x2010, 0x2060);
    bytes[0x2080..0x2080 + module.len()].copy_from_slice(module.as_bytes());
    for (index, name) in names.iter().enumerate() {
        word(&mut bytes, 0x2040 + index * 4, 0x2100 + index as u32 * 32);
        let at = 0x2102 + index * 32;
        bytes[at..at + name.len()].copy_from_slice(name.as_bytes());
    }
    bytes[0x1000..0x1003].copy_from_slice(&[0x31, 0xc0, 0xc3]);
    bytes
}
