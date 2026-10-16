pub use crate::mobile::TouchAction;
pub(crate) use crate::mobile::position;
/// Android physical key codes to Win32 virtual keys and set-1 scan codes.
pub(crate) fn key(code: i32) -> Option<(u32, u32)> {
    const LETTERS: [u32; 26] = [
        0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18,
        0x19, 0x10, 0x13, 0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
    ];
    Some(match code {
        29..=54 => (0x41 + (code - 29) as u32, LETTERS[(code - 29) as usize]),
        7 => (0x30, 0x0b),
        8..=16 => (0x31 + (code - 8) as u32, (code - 6) as u32),
        131..=142 => (
            0x70 + (code - 131) as u32,
            if code <= 140 {
                (code - 72) as u32
            } else {
                (code - 54) as u32
            },
        ),
        19 => (0x26, 0x148),
        20 => (0x28, 0x150),
        21 => (0x25, 0x14b),
        22 => (0x27, 0x14d),
        57 => (0x12, 0x38),
        58 => (0x12, 0x138),
        59 => (0x10, 0x2a),
        60 => (0x10, 0x36),
        61 => (9, 0x0f),
        62 => (0x20, 0x39),
        66 => (0x0d, 0x1c),
        67 => (8, 0x0e),
        111 => (0x1b, 1),
        112 => (0x2e, 0x153),
        113 => (0x11, 0x1d),
        114 => (0x11, 0x11d),
        115 => (0x14, 0x3a),
        116 => (0x91, 0x46),
        120 => (0x2c, 0x137),
        121 => (0x13, 0x45),
        92 => (0x21, 0x149),
        93 => (0x22, 0x151),
        122 => (0x24, 0x147),
        123 => (0x23, 0x14f),
        124 => (0x2d, 0x152),
        55 => (0xbc, 0x33),
        56 => (0xbe, 0x34),
        68 => (0xc0, 0x29),
        69 => (0xbd, 0x0c),
        70 => (0xbb, 0x0d),
        71 => (0xdb, 0x1a),
        72 => (0xdd, 0x1b),
        73 => (0xdc, 0x2b),
        74 => (0xba, 0x27),
        75 => (0xde, 0x28),
        76 => (0xbf, 0x35),
        _ => return None,
    })
}
pub(crate) use crate::mobile::key_message as message;
#[cfg(test)]
mod tests {
    use super::*;
    // spec: AND-APP-11
    #[test]
    fn physical_keys_preserve_scan_codes_and_leave_back_to_the_host() {
        assert_eq!(key(4), None);
        assert_eq!(key(29), Some((0x41, 0x1e)));
        assert_eq!(key(114), Some((0x11, 0x11d)));
        assert_eq!(
            message(0x11, 0x11d, false, false, false),
            (0x101, 0x11, 0xc11d0001)
        );
        assert_eq!(
            message(0x41, 0x1e, true, true, true),
            (0x104, 0x41, 0x601e0001)
        );
    }
}
