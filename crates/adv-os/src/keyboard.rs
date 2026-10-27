//! Modifier-side mapping for host key messages delivered to Wine.

/// Left or right virtual key behind a generic Shift, Ctrl or Alt message.
/// Shift sides follow the scan code; Ctrl and Alt sides follow the extended bit.
pub fn side_virtual_key(key: u32, lparam: u32) -> Option<u32> {
    let right = match key {
        0x10 => (lparam >> 16) & 0xff == 0x36,
        0x11 | 0x12 => lparam & (1 << 24) != 0,
        _ => return None,
    };
    Some(0xa0 + (key - 0x10) * 2 + u32::from(right))
}
