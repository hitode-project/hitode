//! Authored PE metadata only: malformed counts and ranges must return PeError.
use adv_pe::{DataDir, ExportTarget, Pe, PeError, dir};

const OH: usize = 0x98;
const DIRECTORY: u32 = 0x400;

fn word(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}

fn half(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}

fn file() -> Vec<u8> {
    let mut bytes = vec![0; 0x1000];
    bytes[..2].copy_from_slice(b"MZ");
    word(&mut bytes, 0x3c, 0x80);
    bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
    half(&mut bytes, 0x84, 0x14c);
    half(&mut bytes, 0x94, 224);
    half(&mut bytes, OH, 0x10b);
    word(&mut bytes, OH + 28, 0x0040_0000);
    word(&mut bytes, OH + 32, 0x1000);
    word(&mut bytes, OH + 36, 0x200);
    word(&mut bytes, OH + 56, 0x1000);
    word(&mut bytes, OH + 60, 0x1000);
    word(&mut bytes, OH + 92, 16);
    bytes
}

fn image_pe(index: usize, size: u32) -> Pe {
    let mut pe = Pe::parse(&file()).unwrap();
    pe.dirs[index] = DataDir {
        rva: DIRECTORY,
        size,
    };
    pe
}

#[test]
fn headers_reject_directory_counts_outside_the_optional_header() {
    for count in [17, u32::MAX] {
        let mut bytes = file();
        word(&mut bytes, OH + 92, count);
        assert!(Pe::parse_headers(&bytes).is_err());
    }
    // Extended optional headers remain accepted when the declared entries fit.
    let mut bytes = file();
    half(&mut bytes, 0x94, 232);
    word(&mut bytes, OH + 92, 17);
    assert!(Pe::parse_headers(&bytes).is_ok());
}

#[test]
fn headers_reject_truncated_optional_headers_and_section_tables() {
    for size in [0, 2, 92, 96, 223] {
        let mut bytes = file();
        half(&mut bytes, 0x94, size);
        assert!(Pe::parse_headers(&bytes).is_err());
    }
    let mut bytes = file();
    bytes.truncate(OH + 223);
    assert!(Pe::parse_headers(&bytes).is_err());
    let mut bytes = file();
    half(&mut bytes, 0x86, u16::MAX);
    assert!(Pe::parse_headers(&bytes).is_err());
    word(&mut bytes, 0x3c, u32::MAX);
    assert!(Pe::parse_headers(&bytes).is_err());
}

#[test]
fn headers_validate_directory_spans_before_mapping() {
    for (rva, size) in [(0xfff, 2), (u32::MAX - 1, 4), (DIRECTORY, u32::MAX)] {
        let mut bytes = file();
        word(&mut bytes, OH + 96 + dir::EXPORT * 8, rva);
        word(&mut bytes, OH + 100 + dir::EXPORT * 8, size);
        assert!(Pe::parse_headers(&bytes).is_err());
    }
}

#[test]
fn security_directory_uses_file_offsets_and_last_image_page_remains_valid() {
    let mut bytes = file();
    bytes.resize(0x1100, 0);
    word(&mut bytes, OH + 96 + dir::SECURITY * 8, 0x1000);
    word(&mut bytes, OH + 100 + dir::SECURITY * 8, 0x100);
    assert!(Pe::parse(&bytes).is_ok());
    word(&mut bytes, OH + 100 + dir::SECURITY * 8, 0x101);
    assert!(Pe::parse_headers(&bytes).is_err());
    let mut bytes = file();
    word(&mut bytes, OH + 56, 0xff0);
    word(&mut bytes, OH + 96 + dir::EXPORT * 8, 0xff0);
    word(&mut bytes, OH + 100 + dir::EXPORT * 8, 0x10);
    assert!(Pe::parse(&bytes).is_ok());
}

#[test]
fn relocations_reject_overflowing_or_truncated_directories() {
    for (rva, size) in [(DIRECTORY, u32::MAX), (u32::MAX - 3, 8), (0xffc, 8)] {
        let mut pe = image_pe(dir::BASERELOC, size);
        pe.dirs[dir::BASERELOC].rva = rva;
        assert!(pe.relocations().is_err());
    }
    for size in 1..8 {
        assert!(image_pe(dir::BASERELOC, size).relocations().is_err());
    }
}

#[test]
fn relocations_reject_invalid_block_lengths_and_incomplete_trailing_headers() {
    for size in [0, 7, 9, 12, u32::MAX] {
        let mut pe = image_pe(dir::BASERELOC, 10);
        pe.set_u32(DIRECTORY + 4, size).unwrap();
        assert!(pe.relocations().is_err());
    }
    let mut pe = image_pe(dir::BASERELOC, 11);
    pe.set_u32(DIRECTORY + 4, 10).unwrap();
    assert!(pe.relocations().is_err());
}

#[test]
fn relocations_validate_fixup_targets_before_applying_any_delta() {
    for page in [u32::MAX, 0xffd] {
        let mut pe = image_pe(dir::BASERELOC, 12);
        pe.set_u32(DIRECTORY, page).unwrap();
        pe.set_u32(DIRECTORY + 4, 12).unwrap();
        half(&mut pe.image, DIRECTORY as usize + 8, 0x3001);
        let before = pe.image.clone();
        assert!(pe.relocations().is_err());
        assert!(pe.relocate(1).is_err());
        assert_eq!(pe.image, before);
    }
    let mut pe = image_pe(dir::BASERELOC, 12);
    pe.set_u32(DIRECTORY + 4, 12).unwrap();
    half(&mut pe.image, DIRECTORY as usize + 8, 0x3200);
    half(&mut pe.image, DIRECTORY as usize + 10, 0x3fff);
    let before = pe.image.clone();
    assert!(pe.relocate(1).is_err());
    assert_eq!(pe.image, before);
}

#[test]
fn relocations_preserve_padding_empty_blocks_and_wrapping_deltas() {
    let mut pe = image_pe(dir::BASERELOC, 20);
    pe.set_u32(DIRECTORY + 4, 8).unwrap();
    pe.set_u32(DIRECTORY + 12, 12).unwrap();
    half(&mut pe.image, DIRECTORY as usize + 16, 0x3200);
    pe.set_u32(0x200, u32::MAX).unwrap();
    assert_eq!(pe.relocations().unwrap(), [0x200]);
    assert_eq!(pe.relocate(2).unwrap(), 1);
    assert_eq!(pe.u32_at(0x200).unwrap(), 1);
}

fn export_pe() -> Pe {
    let mut pe = image_pe(dir::EXPORT, 0x80);
    for (offset, value) in [
        (16, 1),
        (20, 2),
        (24, 1),
        (28, 0x600),
        (32, 0x620),
        (36, 0x640),
    ] {
        pe.set_u32(DIRECTORY + offset, value).unwrap();
    }
    pe.set_u32(0x600, 0x800).unwrap();
    pe.set_u32(0x604, DIRECTORY + 0x60).unwrap();
    pe.set_u32(0x620, 0x660).unwrap();
    pe.image[0x660..0x665].copy_from_slice(b"Name\0");
    pe.image[0x460..0x46a].copy_from_slice(b"OTHER.Run\0");
    pe
}

#[test]
fn exports_reject_truncated_directory_headers_and_arrays() {
    let mut pe = export_pe();
    pe.dirs[dir::EXPORT].size = 39;
    assert!(pe.exports().is_err());
    for (field, value) in [(28, 0xffc), (32, 0xffe), (36, 0xfff)] {
        let mut pe = export_pe();
        pe.set_u32(DIRECTORY + field, value).unwrap();
        assert!(pe.exports().is_err());
    }
}

#[test]
fn exports_reject_oversized_counts_before_allocating() {
    for field in [20, 24] {
        let mut pe = export_pe();
        pe.set_u32(DIRECTORY + field, u32::MAX).unwrap();
        assert!(pe.exports().is_err());
    }
}

#[test]
fn exports_reject_invalid_name_indices_and_unrepresentable_ordinals() {
    let mut pe = export_pe();
    half(&mut pe.image, 0x640, 2);
    assert!(pe.exports().is_err());
    for base in [u16::MAX as u32, u32::MAX] {
        let mut pe = export_pe();
        pe.set_u32(DIRECTORY + 16, base).unwrap();
        assert!(pe.exports().is_err());
    }
}

#[test]
fn exports_preserve_names_forwarders_holes_and_the_last_ordinal() {
    let mut pe = export_pe();
    pe.set_u32(DIRECTORY + 16, u16::MAX as u32 - 1).unwrap();
    let exports = pe.exports().unwrap();
    assert_eq!(exports.len(), 2);
    assert_eq!(exports[0].ordinal, u16::MAX - 1);
    assert_eq!(exports[0].name.as_deref(), Some("Name"));
    assert_eq!(exports[0].target, ExportTarget::Rva(0x800));
    assert_eq!(exports[1].ordinal, u16::MAX);
    assert_eq!(
        exports[1].target,
        ExportTarget::Forwarder("OTHER.Run".into())
    );
    pe.set_u32(0x600, 0).unwrap();
    assert_eq!(pe.exports().unwrap().len(), 1);
}

#[test]
fn exports_allow_multiple_names_for_one_slot_and_empty_tables() {
    let mut pe = export_pe();
    pe.set_u32(DIRECTORY + 24, 3).unwrap();
    pe.set_u32(0x624, 0x660).unwrap();
    pe.set_u32(0x628, 0x660).unwrap();
    assert_eq!(pe.exports().unwrap()[0].name.as_deref(), Some("Name"));
    let mut pe = export_pe();
    pe.set_u32(DIRECTORY + 20, 0).unwrap();
    pe.set_u32(DIRECTORY + 24, 0).unwrap();
    assert!(pe.exports().unwrap().is_empty());
}

#[test]
fn imports_delay_imports_and_tls_reject_truncated_directory_records() {
    assert!(image_pe(dir::IMPORT, 19).imports().is_err());
    assert!(image_pe(dir::DELAY_IMPORT, 31).delay_imports().is_err());
    assert!(image_pe(dir::TLS, 23).tls().is_err());
}

#[test]
fn import_thunks_validate_iat_slots() {
    let mut pe = image_pe(dir::IMPORT, 40);
    pe.set_u32(DIRECTORY, 0x600).unwrap();
    pe.set_u32(DIRECTORY + 12, 0x700).unwrap();
    pe.set_u32(DIRECTORY + 16, 0xffe).unwrap();
    pe.image[0x700..0x706].copy_from_slice(b"x.dll\0");
    pe.set_u32(0x600, 0x8000_0001).unwrap();
    assert!(pe.imports().is_err());
}

#[test]
fn unterminated_import_and_delay_descriptor_arrays_are_errors() {
    let mut pe = image_pe(dir::IMPORT, 20);
    pe.set_u32(DIRECTORY + 12, 0x700).unwrap();
    pe.set_u32(DIRECTORY + 16, 0x600).unwrap();
    pe.image[0x700..0x706].copy_from_slice(b"x.dll\0");
    assert!(pe.imports().is_err());
    let mut pe = image_pe(dir::DELAY_IMPORT, 32);
    pe.set_u32(DIRECTORY, 1).unwrap();
    pe.set_u32(DIRECTORY + 4, 0x700).unwrap();
    pe.set_u32(DIRECTORY + 12, 0x600).unwrap();
    pe.set_u32(DIRECTORY + 16, 0x600).unwrap();
    pe.image[0x700..0x706].copy_from_slice(b"x.dll\0");
    assert!(pe.delay_imports().is_err());
}

#[test]
fn public_range_accessors_reject_rva_overflow() {
    let mut pe = Pe::parse(&file()).unwrap();
    assert!(matches!(
        pe.slice(u32::MAX, 2),
        Err(PeError::OutOfImage { .. })
    ));
    assert!(pe.u16_at(u32::MAX).is_err());
    assert!(pe.u32_at(u32::MAX).is_err());
    assert!(pe.set_u32(u32::MAX, 1).is_err());
}

#[test]
fn resources_reject_directory_arrays_and_strings_outside_the_declared_span() {
    let mut pe = image_pe(dir::RESOURCE, 23);
    half(&mut pe.image, DIRECTORY as usize + 14, 1);
    assert!(pe.resources().is_err());
    pe.dirs[dir::RESOURCE].size = 24;
    pe.set_u32(DIRECTORY + 16, 0x8000_0020).unwrap();
    half(&mut pe.image, DIRECTORY as usize + 0x20, 1);
    assert!(pe.resources().is_err());
    pe.dirs[dir::RESOURCE].size = u32::MAX;
    assert!(pe.resources().is_err());
}

#[test]
fn resources_preserve_data_payloads_outside_the_metadata_directory() {
    let mut pe = image_pe(dir::RESOURCE, 40);
    half(&mut pe.image, DIRECTORY as usize + 14, 1);
    pe.set_u32(DIRECTORY + 16, 10).unwrap();
    pe.set_u32(DIRECTORY + 20, 24).unwrap();
    pe.set_u32(DIRECTORY + 24, 0x800).unwrap();
    pe.set_u32(DIRECTORY + 28, 4).unwrap();
    let resource = pe.resources().unwrap().unwrap();
    let adv_pe::ResourceNode::Data(data) = &resource.entries[0].node else {
        panic!("expected resource data");
    };
    assert_eq!(data.rva, 0x800);
    assert_eq!(data.size, 4);
}

#[test]
fn export_forwarders_must_terminate_within_the_directory() {
    let mut pe = export_pe();
    pe.dirs[dir::EXPORT].size = 0x69;
    assert!(pe.exports().is_err());
}

#[test]
fn unterminated_import_thunks_and_tls_callbacks_fail_at_image_end() {
    let mut pe = image_pe(dir::IMPORT, 40);
    pe.set_u32(DIRECTORY, 0xffc).unwrap();
    pe.set_u32(DIRECTORY + 12, 0x700).unwrap();
    pe.set_u32(DIRECTORY + 16, 0xffc).unwrap();
    pe.image[0x700..0x706].copy_from_slice(b"x.dll\0");
    pe.set_u32(0xffc, 0x8000_0001).unwrap();
    assert!(pe.imports().is_err());
    let tls = adv_pe::Tls {
        raw_start_va: 0,
        raw_end_va: 0,
        index_va: 0,
        callbacks_va: pe.image_base + 0xffc,
        zero_fill: 0,
    };
    assert!(pe.tls_callbacks(&tls, pe.image_base).is_err());
}
