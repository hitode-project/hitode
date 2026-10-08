//! Authored PE metadata only: checked bounds preserve tolerant loader behavior.
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
fn headers_clamp_directory_counts_to_present_entries_and_sixteen() {
    for (size, available) in [(96, 0), (223, 15), (224, 16), (232, 16)] {
        for count in [17, u32::MAX] {
            let mut bytes = file();
            half(&mut bytes, 0x94, size);
            word(&mut bytes, OH + 92, count);
            word(&mut bytes, OH + 96 + dir::IMPORT * 8, DIRECTORY);
            word(&mut bytes, OH + 100 + dir::IMPORT * 8, 40);
            word(&mut bytes, OH + 96 + 15 * 8, DIRECTORY + 0x80);
            word(&mut bytes, OH + 100 + 15 * 8, 40);
            let pe = Pe::parse_headers(&bytes).unwrap();
            assert_eq!(pe.dir(dir::IMPORT).is_present(), available > 1);
            assert_eq!(pe.dir(15).is_present(), available > 15);
        }
    }
}

#[test]
fn headers_reject_truncated_optional_headers_and_section_tables() {
    let mut bytes = file();
    bytes.truncate(OH + 95);
    assert!(Pe::parse_headers(&bytes).is_err());
    let mut bytes = file();
    half(&mut bytes, 0x86, u16::MAX);
    assert!(Pe::parse_headers(&bytes).is_err());
    word(&mut bytes, 0x3c, u32::MAX);
    assert!(Pe::parse_headers(&bytes).is_err());
}

#[test]
fn compatibility_optional_header_reads_only_complete_available_directories() {
    let mut bytes = file();
    bytes.truncate(OH + 223);
    let pe = Pe::parse_headers(&bytes).unwrap();
    assert_eq!(pe.sections.len(), 0);
    assert!(!pe.dir(15).is_present());
    for size in [0, 2, 92] {
        let mut bytes = file();
        half(&mut bytes, 0x94, size);
        assert!(
            Pe::parse_headers(&bytes)
                .unwrap()
                .dirs
                .iter()
                .all(|dir| !dir.is_present())
        );
    }
}

#[test]
fn compatibility_tls_callback_array_can_use_the_final_mapped_page() {
    let mut bytes = file();
    word(&mut bytes, OH + 56, 0xff0);
    let pe = Pe::parse(&bytes).unwrap();
    let tls = adv_pe::Tls {
        raw_start_va: 0,
        raw_end_va: 0,
        index_va: 0,
        callbacks_va: pe.image_base + 0xffc,
        zero_fill: 0,
    };
    assert!(pe.tls_callbacks(&tls, pe.image_base).unwrap().is_empty());
}

#[test]
fn headers_preserve_unused_directory_metadata_without_rejecting_mapping() {
    for (rva, size) in [(0xfff, 2), (u32::MAX - 1, 4), (DIRECTORY, u32::MAX)] {
        let mut bytes = file();
        word(&mut bytes, OH + 96 + dir::EXPORT * 8, rva);
        word(&mut bytes, OH + 100 + dir::EXPORT * 8, size);
        let pe = Pe::parse(&bytes).unwrap();
        assert!(pe.exports().is_ok());
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
    assert!(Pe::parse(&bytes).is_ok());
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
        assert!(
            image_pe(dir::BASERELOC, size)
                .relocations()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn relocations_reject_invalid_block_ranges_and_ignore_padding_or_terminators() {
    for size in [7, 12, u32::MAX] {
        let mut pe = image_pe(dir::BASERELOC, 10);
        pe.set_u32(DIRECTORY + 4, size).unwrap();
        assert!(pe.relocations().is_err());
    }
    let mut pe = image_pe(dir::BASERELOC, 11);
    pe.set_u32(DIRECTORY + 4, 10).unwrap();
    assert!(pe.relocations().unwrap().is_empty());
    for size in [0, 9] {
        pe.set_u32(DIRECTORY + 4, size).unwrap();
        assert!(pe.relocations().unwrap().is_empty());
    }
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
fn exports_ignore_advisory_sizes_and_unusable_arrays() {
    let mut pe = export_pe();
    pe.dirs[dir::EXPORT].size = 39;
    assert_eq!(pe.exports().unwrap().len(), 2);
    for (field, value) in [(28, 0xffc), (32, 0xffe), (36, 0xfff)] {
        let mut pe = export_pe();
        pe.set_u32(DIRECTORY + field, value).unwrap();
        let exports = pe.exports().unwrap();
        assert_eq!(exports.len(), if field == 28 { 0 } else { 2 });
    }
}

#[test]
fn exports_ignore_oversized_counts_without_oversized_allocations() {
    for field in [20, 24] {
        let mut pe = export_pe();
        pe.set_u32(DIRECTORY + field, u32::MAX).unwrap();
        assert!(pe.exports().unwrap().len() <= 2);
    }
}

#[test]
fn exports_skip_unrepresentable_ordinals_without_creating_aliases() {
    let mut pe = export_pe();
    pe.set_u32(DIRECTORY + 16, u16::MAX as u32).unwrap();
    let exports = pe.exports().unwrap();
    assert_eq!(exports.len(), 1);
    assert_eq!(exports[0].ordinal, u16::MAX);
    pe.set_u32(DIRECTORY + 16, u32::MAX).unwrap();
    assert!(pe.exports().unwrap().is_empty());
}

#[test]
fn exports_ignore_invalid_name_indices_without_reading_the_name() {
    let mut pe = export_pe();
    half(&mut pe.image, 0x640, 2);
    pe.set_u32(0x620, u32::MAX).unwrap();
    let exports = pe.exports().unwrap();
    assert_eq!(exports.len(), 2);
    assert!(exports.iter().all(|export| export.name.is_none()));
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
fn imports_delay_imports_and_tls_check_actual_records_not_advisory_sizes() {
    assert!(image_pe(dir::IMPORT, 19).imports().is_ok());
    assert!(image_pe(dir::DELAY_IMPORT, 31).delay_imports().is_ok());
    assert!(image_pe(dir::TLS, 1).tls().is_ok());
    let mut pe = image_pe(dir::IMPORT, 20);
    pe.image.truncate(DIRECTORY as usize + 19);
    assert!(pe.imports().is_err());
    let mut pe = image_pe(dir::DELAY_IMPORT, 32);
    pe.image.truncate(DIRECTORY as usize + 31);
    assert!(pe.delay_imports().is_err());
    let mut pe = image_pe(dir::TLS, 24);
    pe.image.truncate(DIRECTORY as usize + 19);
    assert!(pe.tls().is_err());
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
fn import_thunks_do_not_require_an_unused_iat_terminator_slot() {
    let mut pe = image_pe(dir::IMPORT, 40);
    pe.set_u32(DIRECTORY, 0x600).unwrap();
    pe.set_u32(DIRECTORY + 12, 0x700).unwrap();
    pe.set_u32(DIRECTORY + 16, 0xffc).unwrap();
    pe.image[0x700..0x706].copy_from_slice(b"x.dll\0");
    pe.set_u32(0x600, 0x8000_0001).unwrap();
    assert_eq!(pe.imports().unwrap()[0].imports[0].iat_rva, 0xffc);
    pe.set_u32(0x600, 0).unwrap();
    pe.set_u32(DIRECTORY + 16, u32::MAX).unwrap();
    assert!(pe.imports().unwrap()[0].imports.is_empty());
}

#[test]
fn import_and_delay_descriptors_follow_terminators_beyond_advertised_size() {
    let mut pe = image_pe(dir::IMPORT, 20);
    pe.set_u32(DIRECTORY + 12, 0x700).unwrap();
    pe.set_u32(DIRECTORY + 16, 0x600).unwrap();
    pe.image[0x700..0x706].copy_from_slice(b"x.dll\0");
    assert_eq!(pe.imports().unwrap().len(), 1);
    let mut pe = image_pe(dir::DELAY_IMPORT, 32);
    pe.set_u32(DIRECTORY, 1).unwrap();
    pe.set_u32(DIRECTORY + 4, 0x700).unwrap();
    pe.set_u32(DIRECTORY + 12, 0x600).unwrap();
    pe.set_u32(DIRECTORY + 16, 0x600).unwrap();
    pe.image[0x700..0x706].copy_from_slice(b"x.dll\0");
    assert_eq!(pe.delay_imports().unwrap().len(), 1);
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
fn resources_allow_metadata_outside_advisory_directory_sizes() {
    let mut pe = image_pe(dir::RESOURCE, 23);
    half(&mut pe.image, DIRECTORY as usize + 14, 1);
    assert!(pe.resources().is_ok());
    pe.dirs[dir::RESOURCE].size = 24;
    pe.set_u32(DIRECTORY + 16, 0x8000_0020).unwrap();
    half(&mut pe.image, DIRECTORY as usize + 0x20, 1);
    assert!(pe.resources().is_ok());
    pe.dirs[dir::RESOURCE].size = u32::MAX;
    assert!(pe.resources().is_ok());
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
fn export_forwarders_may_terminate_past_advisory_directory_size() {
    let mut pe = export_pe();
    pe.dirs[dir::EXPORT].size = 0x69;
    assert_eq!(pe.exports().unwrap().len(), 2);
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
