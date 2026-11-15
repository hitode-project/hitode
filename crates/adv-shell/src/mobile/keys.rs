//! Win32 keyboard messages for physical and overlay keys on mobile hosts.

const LETTERS: [u32; 26] = [
    0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19,
    0x10, 0x13, 0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
];

/// The set-1 scan code of a virtual key the overlay sends, with bit 8 set for
/// extended keys so `key_message` sets bit 24 of `lParam`.
pub fn overlay_scan_code(key: u32) -> Option<u32> {
    Some(match key {
        0x08 => 0x0e,
        0x09 => 0x0f,
        0x0d => 0x1c,
        0x10 => 0x2a,
        0x11 => 0x1d,
        0x12 => 0x38,
        0x13 => 0x45,
        0x14 => 0x3a,
        0x1b => 0x01,
        0x20 => 0x39,
        0x21 => 0x149,
        0x22 => 0x151,
        0x23 => 0x14f,
        0x24 => 0x147,
        0x25 => 0x14b,
        0x26 => 0x148,
        0x27 => 0x14d,
        0x28 => 0x150,
        0x2d => 0x152,
        0x2e => 0x153,
        0x30 => 0x0b,
        0x31..=0x39 => key - 0x31 + 0x02,
        0x41..=0x5a => LETTERS[(key - 0x41) as usize],
        0x70..=0x79 => key - 0x70 + 0x3b,
        0x7a => 0x57,
        0x7b => 0x58,
        0xba => 0x27,
        0xbb => 0x0d,
        0xbc => 0x33,
        0xbd => 0x0c,
        0xbe => 0x34,
        0xbf => 0x35,
        0xc0 => 0x29,
        0xdb => 0x1a,
        0xdc => 0x2b,
        0xdd => 0x1b,
        0xde => 0x28,
        _ => return None,
    })
}

/// WM_(SYS)KEYDOWN/UP for a virtual key and set-1 scan code.
pub fn key_message(key: u32, scan: u32, down: bool, repeat: bool, alt: bool) -> (u32, u32, u32) {
    (
        if down { 0x100 } else { 0x101 } + if alt || key == 0x79 { 4 } else { 0 },
        key,
        1 | scan << 16
            | u32::from(alt) << 29
            | u32::from(repeat || !down) << 30
            | u32::from(!down) << 31,
    )
}

#[cfg(test)]
mod tests {
    use super::{key_message, overlay_scan_code};

    // spec: HOST-VIEW-5
    #[test]
    fn overlay_scan_code_maps_supported_virtual_keys() {
        assert_eq!(overlay_scan_code(0x08), Some(0x0e));
        assert_eq!(overlay_scan_code(0x09), Some(0x0f));
        assert_eq!(overlay_scan_code(0x0d), Some(0x1c));
        assert_eq!(overlay_scan_code(0x10), Some(0x2a));
        assert_eq!(overlay_scan_code(0x11), Some(0x1d));
        assert_eq!(overlay_scan_code(0x12), Some(0x38));
        assert_eq!(overlay_scan_code(0x1b), Some(0x01));
        assert_eq!(overlay_scan_code(0x20), Some(0x39));
        assert_eq!(overlay_scan_code(0x25), Some(0x14b));
        assert_eq!(overlay_scan_code(0x26), Some(0x148));
        assert_eq!(overlay_scan_code(0x30), Some(0x0b));
        assert_eq!(overlay_scan_code(0x31), Some(0x02));
        assert_eq!(overlay_scan_code(0x41), Some(0x1e));
        assert_eq!(overlay_scan_code(0x5a), Some(0x2c));
        assert_eq!(overlay_scan_code(0x70), Some(0x3b));
        assert_eq!(overlay_scan_code(0x7b), Some(0x58));
        assert_eq!(overlay_scan_code(0x5b), None);
    }

    // spec: HOST-VIEW-5
    #[test]
    fn overlay_virtual_keys_have_unique_scan_codes_and_extended_lparam() {
        let mut seen = std::collections::BTreeMap::new();
        for vk in 0..=0xff {
            if let Some(scan) = overlay_scan_code(vk) {
                assert_eq!(seen.insert(scan, vk), None, "duplicate scan {scan:#x}");
            }
        }
        let left = overlay_scan_code(0x25).unwrap();
        assert_eq!(
            key_message(0x25, left, true, false, false),
            (0x100, 0x25, 0x014b_0001)
        );
        assert_eq!(
            key_message(0x0d, 0x1c, true, false, true),
            (0x104, 0x0d, 0x201c_0001)
        );
    }
}
