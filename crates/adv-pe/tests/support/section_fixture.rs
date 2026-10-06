// Authored PE32 sections, matching tools/fixtures/pe-padding-native.c cases.
pub const BASE: u32 = 0x400000;
pub const CODE: [u8; 6] = [0xb8, 0x44, 0x33, 0x22, 0x11, 0xc3];
pub const TEXT_SECTION: usize = 0x178;

pub fn word(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
fn half(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
pub fn image(virtual_size: u32, raw_size: u32, next: Option<u32>, image_size: u32) -> Vec<u8> {
    let mut file = vec![0; 0x200 + raw_size as usize + if next.is_some() { 0x200 } else { 0 }];
    file[..2].copy_from_slice(b"MZ");
    word(&mut file, 0x3c, 0x80);
    file[0x80..0x84].copy_from_slice(b"PE\0\0");
    half(&mut file, 0x84, 0x14c);
    half(&mut file, 0x86, if next.is_some() { 2 } else { 1 });
    half(&mut file, 0x94, 224);
    half(&mut file, 0x96, 0x103);
    half(&mut file, 0x98, 0x10b);
    word(&mut file, 0x98 + 16, 0x1180);
    word(&mut file, 0x98 + 28, BASE);
    word(&mut file, 0x98 + 32, 0x1000);
    word(&mut file, 0x98 + 36, 0x200);
    word(&mut file, 0x98 + 56, image_size);
    word(&mut file, 0x98 + 60, 0x200);
    half(&mut file, 0x98 + 68, 3);
    word(&mut file, 0x98 + 92, 16);
    file[TEXT_SECTION..TEXT_SECTION + 2].copy_from_slice(b".t");
    word(&mut file, TEXT_SECTION + 8, virtual_size);
    word(&mut file, TEXT_SECTION + 12, 0x1000);
    word(&mut file, TEXT_SECTION + 16, raw_size);
    word(&mut file, TEXT_SECTION + 20, 0x200);
    word(&mut file, TEXT_SECTION + 36, 0x60000020);
    for (offset, byte) in file[0x200..0x200 + raw_size as usize]
        .iter_mut()
        .enumerate()
    {
        *byte = if offset < 0x1000 { 0xcd } else { 0xab };
    }
    for offset in (0x180..raw_size as usize).step_by(0x1000) {
        if offset + CODE.len() <= raw_size as usize {
            file[0x200 + offset..0x200 + offset + CODE.len()].copy_from_slice(&CODE);
        }
    }
    if let Some(next) = next {
        let section = TEXT_SECTION + 40;
        file[section..section + 2].copy_from_slice(b".d");
        word(&mut file, section + 8, 0x100);
        word(&mut file, section + 12, next);
        word(&mut file, section + 16, 0x200);
        word(&mut file, section + 20, 0x200 + raw_size);
        word(&mut file, section + 36, 0x40000040);
        file[0x200 + raw_size as usize..].fill(0x5a);
    }
    file
}
