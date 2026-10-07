//! Windows SEC_IMAGE copies the section's same-page raw tail, but does not let
//! oversized raw data enlarge the declared virtual page extent.
use adv_pe::{Pe, PeError};
#[path = "support/section_fixture.rs"]
mod fixture;

#[test]
fn raw_padding_preserves_executable_and_data_bytes_after_virtual_size() {
    let pe = Pe::parse(&fixture::image(0x101, 0x200, Some(0x2000), 0x3000)).unwrap();
    assert_eq!(pe.sections[0].mem_size(), 0x101);
    assert_eq!(&pe.image[0x1180..0x1186], &fixture::CODE);
    assert_eq!(pe.image[0x1101], 0xcd);
    assert!(pe.image[0x1200..0x2000].iter().all(|byte| *byte == 0));
    assert!(pe.image[0x2100..0x2200].iter().all(|byte| *byte == 0x5a));
}

#[test]
fn oversized_raw_pages_do_not_overwrite_or_fill_the_following_section() {
    let pe = Pe::parse(&fixture::image(0x101, 0x2000, Some(0x2000), 0x3000)).unwrap();
    assert_eq!(pe.sections[0].mem_size(), 0x101);
    assert_eq!(pe.image[0x1fff], 0xcd);
    assert!(pe.image[0x2000..0x2200].iter().all(|byte| *byte == 0x5a));
    assert!(pe.image[0x2200..0x3000].iter().all(|byte| *byte == 0));
}

#[test]
fn zero_virtual_size_uses_raw_extent_and_virtual_tail_is_zero_filled() {
    let pe = Pe::parse(&fixture::image(0, 0x2000, Some(0x3000), 0x4000)).unwrap();
    assert_eq!(pe.sections[0].mem_size(), 0x2000);
    assert_eq!(pe.image[0x2100], 0xab);
    assert_eq!(&pe.image[0x2180..0x2186], &fixture::CODE);
    let pe = Pe::parse(&fixture::image(0x1800, 0x200, Some(0x3000), 0x4000)).unwrap();
    assert!(pe.image[0x1200..0x3000].iter().all(|byte| *byte == 0));
    assert_eq!(pe.image[0x3000], 0x5a);
}

#[test]
fn extra_raw_data_may_exceed_image_size_when_its_virtual_pages_fit() {
    let file = fixture::image(0x101, 0x2000, None, 0x2000);
    let pe = Pe::parse(&file).unwrap();
    assert_eq!(pe.image.len(), 0x2000);
    assert_eq!(&pe.image[0x1180..0x1186], &fixture::CODE);
    assert_eq!(pe.image[0x1fff], 0xcd);
    let mut file = fixture::image(0x101, 0x200, None, 0x2000);
    fixture::word(&mut file, fixture::TEXT_SECTION + 16, u32::MAX);
    let pe = Pe::parse(&file).unwrap();
    assert_eq!(pe.image[0x11ff], 0xcd);
    assert_eq!(pe.image[0x1200], 0);
}

#[test]
fn unaligned_image_size_keeps_the_last_virtual_page_without_rewriting_headers() {
    // Native SEC_IMAGE accepts both the exact logical extent and one byte less
    // when they still reserve the same final page. The raw tail is observable.
    for size in [0x2100, 0x20ff] {
        let pe = Pe::parse(&fixture::image(0x101, 0x200, Some(0x2000), size)).unwrap();
        assert_eq!(pe.size_of_image, size);
        assert_eq!(pe.image.len(), 0x3000);
        assert_eq!(
            u32::from_le_bytes(pe.image[0x98 + 56..0x98 + 60].try_into().unwrap()),
            size
        );
        assert!(pe.image[0x2000..0x2200].iter().all(|byte| *byte == 0x5a));
        assert!(pe.image[0x2200..].iter().all(|byte| *byte == 0));
    }
    let mut crossing = fixture::image(0x101, 0x200, Some(0x2000), 0x2100);
    fixture::word(&mut crossing, fixture::TEXT_SECTION + 40 + 8, 0x1100);
    assert!(matches!(Pe::parse(&crossing), Err(PeError::Malformed(_))));
    assert!(Pe::parse(&fixture::image(0x101, 0x200, Some(0x2000), 0x2000)).is_err());
}

#[test]
fn virtual_extent_image_bounds_and_rounding_overflow_are_rejected() {
    let cases = [
        fixture::image(0x1800, 0x200, None, 0x2000),
        fixture::image(0, 0x2000, None, 0x2000),
        fixture::image(u32::MAX, 0x200, None, 0x2000),
    ];
    for file in cases {
        assert!(matches!(Pe::parse(&file), Err(PeError::Malformed(_))));
    }
    let mut file = fixture::image(0x101, 0x200, None, 0x2000);
    fixture::word(&mut file, fixture::TEXT_SECTION + 12, u32::MAX - 0xfff);
    assert!(matches!(Pe::parse(&file), Err(PeError::Malformed(_))));
}
