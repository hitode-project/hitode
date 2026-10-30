use super::*;
// spec: WIN32-GDI-12
#[test]
fn encoded_strings_keep_guest_codepage_and_complete_dbcs_characters() {
    for (codepage, expected) in [
        (932, &b"\x93\xfa\x96\x7b\x8c\xea"[..]),
        (936, &b"\xc8\xd5\xb1\xbe\xd5\x5a"[..]),
    ] {
        let mut bytes = [0; 8];
        put_string(adv_os::Codec::new(codepage), &mut bytes, "日本語文", false);
        assert_eq!(&bytes[..6], expected);
        assert_eq!(&bytes[6..], &[0, 0]);
    }
    let mut wide = [0; 6];
    put_string(adv_os::Codec::new(932), &mut wide, "😀a", true);
    assert_eq!(&wide[..4], &[0x3d, 0xd8, 0x00, 0xde]);
    assert_eq!(&wide[4..], &[0, 0]);
}

// spec: WIN32-GDI-12
#[test]
fn full_enumerations_list_faces_that_fit_ansi_buffers() {
    let codec = adv_os::Codec::new(932);
    assert!(listed(&codec, "ＭＳ Ｐゴシック"));
    // 28 bytes in Shift_JIS, then 36: engines copy these into 32-byte buffers.
    assert!(listed(&codec, "游明朝体+36ポかな ミディアム"));
    assert!(!listed(&codec, "游明朝体+36ポかな エクストラボールド"));
    assert!(!listed(&codec, "Avenir Next Condensed Ultra Light"));
}
