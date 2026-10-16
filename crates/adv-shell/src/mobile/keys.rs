//! Win32 keyboard messages for physical keys on mobile hosts.

/// The set-1 scan code of a virtual key the overlay sends.
pub fn overlay_scan_code(key: u32) -> Option<u32> {
    Some(match key {
        0x09 => 0x0f,
        0x0d => 0x1c,
        0x11 => 0x1d,
        0x1b => 0x01,
        0x20 => 0x39,
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
