//! Authored PE32 metadata: native image mappings publish their actual load base.
use adv_pe::{Pe, dir, file_flags};

const OPTIONAL: usize = 0x98;
const PREFERRED: u32 = 0x1000_0000;
const LOADED: u32 = 0x2000_0000;
const IMAGE_SIZE: u32 = 0x3000;
const IMAGE_BASE_FIELD: u32 = OPTIONAL as u32 + 28;
const FIXUP: u32 = 0x1000;
const RELOC: u32 = 0x1100;

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

/// One authored section and one HIGHLOW fixup, matching the native control.
fn file(relocations: bool) -> Vec<u8> {
    let mut bytes = vec![0; 0x400];
    bytes[..2].copy_from_slice(b"MZ");
    put32(&mut bytes, 0x3c, 0x80);
    bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
    put16(&mut bytes, 0x84, 0x14c);
    put16(&mut bytes, 0x86, 1);
    put16(&mut bytes, 0x94, 224);
    put16(&mut bytes, 0x96, 0x2102);
    put16(&mut bytes, OPTIONAL, 0x10b);
    for (offset, value) in [
        (28, PREFERRED),
        (32, 0x1000),
        (36, 0x200),
        (56, IMAGE_SIZE),
        (60, 0x200),
        (92, 16),
    ] {
        put32(&mut bytes, OPTIONAL + offset, value);
    }
    for (offset, value) in [(40, 5), (48, 5), (68, 2), (70, 0x40)] {
        put16(&mut bytes, OPTIONAL + offset, value);
    }
    if relocations {
        put32(&mut bytes, OPTIONAL + 96 + dir::BASERELOC * 8, RELOC);
        put32(&mut bytes, OPTIONAL + 100 + dir::BASERELOC * 8, 12);
    }
    let section = OPTIONAL + 224;
    bytes[section..section + 5].copy_from_slice(b".text");
    for (offset, value) in [
        (8, IMAGE_SIZE - 0x1000),
        (12, 0x1000),
        (16, 0x200),
        (20, 0x200),
        (36, 0xe000_0020),
    ] {
        put32(&mut bytes, section + offset, value);
    }
    put32(&mut bytes, 0x200, PREFERRED + FIXUP + 4);
    put32(&mut bytes, 0x300, FIXUP);
    put32(&mut bytes, 0x304, 12);
    put16(&mut bytes, 0x308, 0x3000);
    bytes
}

#[test]
fn rebase_updates_the_mapped_header_and_highlow_target() {
    let bytes = file(true);
    let mut pe = Pe::parse(&bytes).unwrap();
    assert_eq!(pe.rebase(LOADED).unwrap(), 1);
    assert_eq!(pe.u32_at(IMAGE_BASE_FIELD).unwrap(), LOADED);
    assert_eq!(pe.u32_at(FIXUP).unwrap(), LOADED + FIXUP + 4);
    assert_eq!(pe.relocations().unwrap(), [FIXUP]);
    // Rebasing the mapped buffer never edits the source file.
    assert_eq!(
        u32::from_le_bytes(bytes[IMAGE_BASE_FIELD as usize..][..4].try_into().unwrap()),
        PREFERRED
    );
}

#[test]
fn rebase_without_a_directory_still_updates_the_header() {
    let mut pe = Pe::parse(&file(false)).unwrap();
    assert_eq!(pe.characteristics & file_flags::RELOCS_STRIPPED, 0);
    assert!(!pe.dir(dir::BASERELOC).is_present());
    assert_eq!(pe.rebase(LOADED).unwrap(), 0);
    assert_eq!(pe.u32_at(IMAGE_BASE_FIELD).unwrap(), LOADED);
    assert_eq!(pe.u32_at(FIXUP).unwrap(), PREFERRED + FIXUP + 4);
}

#[test]
fn invalid_rebase_preserves_the_header_and_all_fixups() {
    for case in 0..5 {
        let mut pe = Pe::parse(&file(true)).unwrap();
        match case {
            0 => {
                // A valid first block precedes an out-of-image target.
                pe.dirs[dir::BASERELOC].size = 24;
                pe.set_u32(RELOC + 12, IMAGE_SIZE).unwrap();
                pe.set_u32(RELOC + 16, 12).unwrap();
                put16(&mut pe.image, RELOC as usize + 20, 0x3000);
            }
            1 => {
                // The first HIGHLOW is valid, but its following type is not.
                put16(&mut pe.image, RELOC as usize + 10, 0xa000);
            }
            2 => pe.dirs[dir::BASERELOC].rva = IMAGE_SIZE - 4,
            3 => pe.nt_headers = IMAGE_SIZE - 4,
            4 => pe.nt_headers = u32::MAX - 4,
            _ => unreachable!(),
        }
        let before = pe.image.clone();
        let before_base = pe.image_base;
        assert!(pe.rebase(LOADED).is_err(), "case {case}");
        assert_eq!(pe.image, before, "case {case}");
        assert_eq!(pe.image_base, before_base, "case {case}");
    }
}
