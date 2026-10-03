// Authored PE32 image: one named and one ordinal delay import, plus HIGHLOW
// fixups for every old-format pointer. No game data or executable instructions.
pub const BASE: u32 = 0x0040_0000;
pub const DESC: u32 = 0x100;
pub const INT: u32 = 0x200;
pub const IAT: u32 = 0x240;
pub const HMOD: u32 = 0x280;

pub fn word(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

pub fn image(rva_based: bool) -> Vec<u8> {
    let mut bytes = vec![0; 4096];
    bytes[..2].copy_from_slice(b"MZ");
    word(&mut bytes, 0x3c, 0x80);
    bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
    bytes[0x84..0x86].copy_from_slice(&0x14cu16.to_le_bytes());
    bytes[0x94..0x96].copy_from_slice(&224u16.to_le_bytes());
    bytes[0x96..0x98].copy_from_slice(&0x2000u16.to_le_bytes());
    bytes[0x98..0x9a].copy_from_slice(&0x10bu16.to_le_bytes());
    word(&mut bytes, 0x98 + 28, BASE);
    word(&mut bytes, 0x98 + 32, 4096);
    word(&mut bytes, 0x98 + 36, 512);
    word(&mut bytes, 0x98 + 56, 4096);
    word(&mut bytes, 0x98 + 60, 4096);
    word(&mut bytes, 0x98 + 92, 16);
    // Use the area after the headers so directory metadata is not overwritten.
    let descriptor = DESC + 0x300;
    word(&mut bytes, 0x98 + 96 + 13 * 8, descriptor);
    word(&mut bytes, 0x98 + 100 + 13 * 8, 64);
    let pointer = |rva| if rva_based { rva } else { BASE + rva };
    word(&mut bytes, descriptor as usize, u32::from(rva_based));
    word(&mut bytes, descriptor as usize + 4, pointer(0x700));
    word(&mut bytes, descriptor as usize + 8, pointer(HMOD + 0x300));
    word(&mut bytes, descriptor as usize + 12, pointer(IAT + 0x300));
    word(&mut bytes, descriptor as usize + 16, pointer(INT + 0x300));
    bytes[0x700..0x70d].copy_from_slice(b"kernel32.dll\0");
    word(&mut bytes, (INT + 0x300) as usize, pointer(0x600));
    word(&mut bytes, (INT + 0x300) as usize + 4, 0x8000_0007);
    bytes[0x600..0x602].copy_from_slice(&42u16.to_le_bytes());
    bytes[0x602..0x60f].copy_from_slice(b"FixtureCall\0\0");
    // The IAT is writable image data; its initial pointers are irrelevant to binding.
    if !rva_based {
        word(&mut bytes, 0x98 + 96 + 5 * 8, 0x800);
        word(&mut bytes, 0x98 + 100 + 5 * 8, 20);
        word(&mut bytes, 0x804, 20);
        for (i, offset) in
            [descriptor + 4, descriptor + 8, descriptor + 12, descriptor + 16, INT + 0x300].into_iter().enumerate()
        {
            bytes[0x808 + i * 2..0x80a + i * 2].copy_from_slice(&(0x3000u16 | offset as u16).to_le_bytes());
        }
    }
    bytes
}
