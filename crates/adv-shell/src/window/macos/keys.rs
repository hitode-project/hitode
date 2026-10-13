//! macOS physical keys as the Win32 keyboard messages of a standard enhanced
//! keyboard. Key codes are positions (`kVK_*`), so each maps to the set 1
//! scan code at the same place and the virtual key a US layout gives it;
//! JIS-only keys keep their Japanese positions.

/// One physical key: the virtual key a message carries, and its scan code
/// with the extended bit as 0xe0 in the high byte.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Key {
    pub vk: u32,
    pub scan: u32,
}

/// Command keys stay with macOS: they drive the host menu, not the guest.
pub(super) fn key(code: u16) -> Option<Key> {
    let (vk, scan) = match code {
        0x00 => (b'A'.into(), 0x1e),
        0x01 => (b'S'.into(), 0x1f),
        0x02 => (b'D'.into(), 0x20),
        0x03 => (b'F'.into(), 0x21),
        0x04 => (b'H'.into(), 0x23),
        0x05 => (b'G'.into(), 0x22),
        0x06 => (b'Z'.into(), 0x2c),
        0x07 => (b'X'.into(), 0x2d),
        0x08 => (b'C'.into(), 0x2e),
        0x09 => (b'V'.into(), 0x2f),
        // The ISO key beside left shift.
        0x0a => (0xe2, 0x56),
        0x0b => (b'B'.into(), 0x30),
        0x0c => (b'Q'.into(), 0x10),
        0x0d => (b'W'.into(), 0x11),
        0x0e => (b'E'.into(), 0x12),
        0x0f => (b'R'.into(), 0x13),
        0x10 => (b'Y'.into(), 0x15),
        0x11 => (b'T'.into(), 0x14),
        0x12 => (b'1'.into(), 0x02),
        0x13 => (b'2'.into(), 0x03),
        0x14 => (b'3'.into(), 0x04),
        0x15 => (b'4'.into(), 0x05),
        0x16 => (b'6'.into(), 0x07),
        0x17 => (b'5'.into(), 0x06),
        0x18 => (0xbb, 0x0d),
        0x19 => (b'9'.into(), 0x0a),
        0x1a => (b'7'.into(), 0x08),
        0x1b => (0xbd, 0x0c),
        0x1c => (b'8'.into(), 0x09),
        0x1d => (b'0'.into(), 0x0b),
        0x1e => (0xdd, 0x1b),
        0x1f => (b'O'.into(), 0x18),
        0x20 => (b'U'.into(), 0x16),
        0x21 => (0xdb, 0x1a),
        0x22 => (b'I'.into(), 0x17),
        0x23 => (b'P'.into(), 0x19),
        0x24 => (0x0d, 0x1c),
        0x25 => (b'L'.into(), 0x26),
        0x26 => (b'J'.into(), 0x24),
        0x27 => (0xde, 0x28),
        0x28 => (b'K'.into(), 0x25),
        0x29 => (0xba, 0x27),
        0x2a => (0xdc, 0x2b),
        0x2b => (0xbc, 0x33),
        0x2c => (0xbf, 0x35),
        0x2d => (b'N'.into(), 0x31),
        0x2e => (b'M'.into(), 0x32),
        0x2f => (0xbe, 0x34),
        0x30 => (0x09, 0x0f),
        0x31 => (0x20, 0x39),
        0x32 => (0xc0, 0x29),
        0x33 => (0x08, 0x0e),
        0x35 => (0x1b, 0x01),
        0x38 => (0x10, 0x2a),
        0x39 => (0x14, 0x3a),
        0x3a => (0x12, 0x38),
        0x3b => (0x11, 0x1d),
        0x3c => (0x10, 0x36),
        0x3d => (0x12, 0xe038),
        0x3e => (0x11, 0xe01d),
        0x40 => (0x80, 0x68),
        0x41 => (0x6e, 0x53),
        0x43 => (0x6a, 0x37),
        0x45 => (0x6b, 0x4e),
        0x4b => (0x6f, 0xe035),
        0x4c => (0x0d, 0xe01c),
        0x4e => (0x6d, 0x4a),
        0x4f => (0x81, 0x69),
        0x50 => (0x82, 0x6a),
        0x52 => (0x60, 0x52),
        0x53 => (0x61, 0x4f),
        0x54 => (0x62, 0x50),
        0x55 => (0x63, 0x51),
        0x56 => (0x64, 0x4b),
        0x57 => (0x65, 0x4c),
        0x58 => (0x66, 0x4d),
        0x59 => (0x67, 0x47),
        0x5a => (0x83, 0x6b),
        0x5b => (0x68, 0x48),
        0x5c => (0x69, 0x49),
        // JIS: yen, underscore (ro), eisu (muhenkan) and kana (henkan).
        0x5d => (0xdc, 0x7d),
        0x5e => (0xe2, 0x73),
        0x66 => (0x1d, 0x7b),
        0x68 => (0x1c, 0x79),
        0x60 => (0x74, 0x3f),
        0x61 => (0x75, 0x40),
        0x62 => (0x76, 0x41),
        0x63 => (0x72, 0x3d),
        0x64 => (0x77, 0x42),
        0x65 => (0x78, 0x43),
        0x67 => (0x7a, 0x57),
        0x69 => (0x7c, 0x64),
        0x6a => (0x7f, 0x67),
        0x6b => (0x7d, 0x65),
        0x6d => (0x79, 0x44),
        0x6e => (0x5d, 0xe05d),
        0x6f => (0x7b, 0x58),
        0x71 => (0x7e, 0x66),
        // Help sits where Insert is on a PC keyboard.
        0x72 => (0x2d, 0xe052),
        0x73 => (0x24, 0xe047),
        0x74 => (0x21, 0xe049),
        0x75 => (0x2e, 0xe053),
        0x76 => (0x73, 0x3e),
        0x77 => (0x23, 0xe04f),
        0x78 => (0x71, 0x3c),
        0x79 => (0x22, 0xe051),
        0x7a => (0x70, 0x3b),
        0x7b => (0x25, 0xe04b),
        0x7c => (0x27, 0xe04d),
        0x7d => (0x28, 0xe050),
        0x7e => (0x26, 0xe048),
        _ => return None,
    };
    Some(Key { vk, scan })
}

pub(super) const WM_KEYDOWN: u32 = 0x100;
pub(super) const WM_KEYUP: u32 = 0x101;
pub(super) const WM_SYSKEYDOWN: u32 = 0x104;
pub(super) const WM_SYSKEYUP: u32 = 0x105;

/// The message, wParam and lParam Windows posts for `key`. `alt` is whether
/// Alt (Option) is held without Ctrl, which makes it a system key.
pub(super) fn message(key: Key, down: bool, repeat: bool, alt: bool) -> (u32, u32, u32) {
    let system = alt || key.vk == 0x79;
    let message = match (down, system) {
        (true, false) => WM_KEYDOWN,
        (false, false) => WM_KEYUP,
        (true, true) => WM_SYSKEYDOWN,
        (false, true) => WM_SYSKEYUP,
    };
    let lparam = 1
        | (key.scan & 0xff) << 16
        | u32::from(key.scan >> 8 == 0xe0) << 24
        | u32::from(alt) << 29
        | u32::from(repeat || !down) << 30
        | u32::from(!down) << 31;
    (message, key.vk, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positions_carry_us_virtual_keys_and_set_1_scan_codes() {
        assert_eq!(
            key(0x00),
            Some(Key {
                vk: 0x41,
                scan: 0x1e
            })
        );
        assert_eq!(
            key(0x24),
            Some(Key {
                vk: 0x0d,
                scan: 0x1c
            })
        );
        // Keypad enter, arrows and the right modifiers are extended.
        assert_eq!(
            key(0x4c),
            Some(Key {
                vk: 0x0d,
                scan: 0xe01c
            })
        );
        assert_eq!(
            key(0x7b),
            Some(Key {
                vk: 0x25,
                scan: 0xe04b
            })
        );
        assert_eq!(
            key(0x3e),
            Some(Key {
                vk: 0x11,
                scan: 0xe01d
            })
        );
        assert_eq!(
            key(0x3b),
            Some(Key {
                vk: 0x11,
                scan: 0x1d
            })
        );
        assert_eq!(
            key(0x3c),
            Some(Key {
                vk: 0x10,
                scan: 0x36
            })
        );
        // Command and Fn belong to macOS.
        assert_eq!(key(0x37), None);
        assert_eq!(key(0x36), None);
        assert_eq!(key(0x3f), None);
    }

    #[test]
    fn no_two_positions_share_a_scan_code() {
        let mut seen = std::collections::BTreeMap::new();
        for code in 0..=0x7f {
            if let Some(key) = key(code) {
                assert_eq!(seen.insert(key.scan, code), None, "scan {:#x}", key.scan);
            }
        }
    }

    #[test]
    fn messages_follow_the_win32_lparam_layout() {
        let ctrl = key(0x3b).unwrap();
        assert_eq!(
            message(ctrl, true, false, false),
            (WM_KEYDOWN, 0x11, 0x001d_0001)
        );
        assert_eq!(
            message(ctrl, true, true, false),
            (WM_KEYDOWN, 0x11, 0x401d_0001)
        );
        assert_eq!(
            message(ctrl, false, false, false),
            (WM_KEYUP, 0x11, 0xc01d_0001)
        );
        let left = key(0x7b).unwrap();
        assert_eq!(message(left, true, false, false).2, 0x014b_0001);
        // Alt+Enter and F10 are system keys.
        let enter = key(0x24).unwrap();
        assert_eq!(
            message(enter, true, false, true),
            (WM_SYSKEYDOWN, 0x0d, 0x201c_0001)
        );
        assert_eq!(
            message(key(0x6d).unwrap(), false, false, false).0,
            WM_SYSKEYUP
        );
    }
}
