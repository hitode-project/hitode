//! Pure scan/virtual-key conversion for the standard enhanced guest keyboard.
//! USER and DirectInput keep their existing key-state owners.

// DirectInput scan indices distinguish numpad positions and use bit 7
// for E0 positions. They differ from MapVirtualKey scan semantics.
pub fn directinput_virtual_key(scan: u8) -> Option<usize> {
    let value = match scan {
        1 => 0x1b,
        2..=10 => b'1' as usize + usize::from(scan - 2),
        11 => b'0' as usize,
        12 => 0xbd,
        13 => 0xbb,
        14 => 8,
        15 => 9,
        16 => b'Q' as usize,
        17 => b'W' as usize,
        18 => b'E' as usize,
        19 => b'R' as usize,
        20 => b'T' as usize,
        21 => b'Y' as usize,
        22 => b'U' as usize,
        23 => b'I' as usize,
        24 => b'O' as usize,
        25 => b'P' as usize,
        26 => 0xdb,
        27 => 0xdd,
        28 => 0x0d,
        29 => 0xa2,
        30 => b'A' as usize,
        31 => b'S' as usize,
        32 => b'D' as usize,
        33 => b'F' as usize,
        34 => b'G' as usize,
        35 => b'H' as usize,
        36 => b'J' as usize,
        37 => b'K' as usize,
        38 => b'L' as usize,
        39 => 0xba,
        40 => 0xde,
        41 => 0xc0,
        42 => 0xa0,
        43 => 0xdc,
        44 => b'Z' as usize,
        45 => b'X' as usize,
        46 => b'C' as usize,
        47 => b'V' as usize,
        48 => b'B' as usize,
        49 => b'N' as usize,
        50 => b'M' as usize,
        51 => 0xbc,
        52 => 0xbe,
        53 => 0xbf,
        54 => 0xa1,
        55 => 0x6a,
        56 => 0xa4,
        57 => 0x20,
        58 => 0x14,
        59..=68 => 0x70 + usize::from(scan - 59),
        69 => 0x90,
        70 => 0x91,
        71 => 0x67,
        72 => 0x68,
        73 => 0x69,
        74 => 0x6d,
        75 => 0x64,
        76 => 0x65,
        77 => 0x66,
        78 => 0x6b,
        79 => 0x61,
        80 => 0x62,
        81 => 0x63,
        82 => 0x60,
        83 => 0x6e,
        87 => 0x7a,
        88 => 0x7b,
        0x9d => 0xa3,
        0xb5 => 0x6f,
        0xb7 => 0x2c,
        0xb8 => 0xa5,
        0xc5 => 0x13,
        0xc7 => 0x24,
        0xc8 => 0x26,
        0xc9 => 0x21,
        0xcb => 0x25,
        0xcd => 0x27,
        0xcf => 0x23,
        0xd0 => 0x28,
        0xd1 => 0x22,
        0xd2 => 0x2d,
        0xd3 => 0x2e,
        0xdb => 0x5b,
        0xdc => 0x5c,
        0xdd => 0x5d,
        _ => return None,
    };
    Some(value)
}

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

/// Map the standard enhanced guest keyboard for its retained input locale.
/// `None` denotes an unavailable profile/type; an unmapped key is `Some(0)`.
pub fn map_virtual_key(layout: u32, code: u32, kind: u32) -> Option<u32> {
    let japanese = japanese_profile(layout)?;
    Some(match kind {
        0 | 4 => {
            let scan = virtual_key_scan(code, japanese);
            if kind == 4 {
                scan
            } else if code == 0x13 {
                0
            } else {
                scan & 0xff
            }
        }
        1 | 3 => {
            let vk = scan_virtual_key(code, japanese);
            if kind == 1 {
                match vk {
                    0xa0 | 0xa1 => 0x10,
                    0xa2 | 0xa3 => 0x11,
                    0xa4 | 0xa5 => 0x12,
                    _ => vk,
                }
            } else {
                vk
            }
        }
        2 => virtual_key_character(code),
        _ => return None,
    })
}

fn japanese_profile(layout: u32) -> Option<bool> {
    match layout {
        0x0411_0411 => true,
        0x0409_0409 | 0x0804_0804 => false,
        _ => return None,
    }
    .into()
}

/// Names for the same enhanced keyboard. Labels follow its native layout table;
/// unknown positions have an empty name, while an unknown profile is unavailable.
pub fn key_name(layout: u32, parameter: u32) -> Option<String> {
    let japanese = japanese_profile(layout)?;
    let mut scan = (parameter >> 16) & 0xff;
    let mut extended = parameter & (1 << 24) != 0;
    if parameter & (1 << 25) != 0 {
        if scan == 0x36 {
            scan = 0x2a;
        }
        if matches!(scan, 0x1d | 0x38) {
            extended = false;
        }
    }
    let label = if extended {
        match scan {
            0x1c => "Num Enter",
            0x1d if japanese => "Right Control",
            0x1d => "Right Ctrl",
            0x35 => "Num /",
            0x37 => "Prnt Scrn",
            0x38 => "Right Alt",
            0x45 => "Num Lock",
            0x46 => "Break",
            0x47 => "Home",
            0x48 => "Up",
            0x49 => "Page Up",
            0x4b => "Left",
            0x4d => "Right",
            0x4f => "End",
            0x50 => "Down",
            0x51 => "Page Down",
            0x52 => "Insert",
            0x53 => "Delete",
            0x54 if !japanese => "<00>",
            0x56 if !japanese => "Help",
            0x5b => "Left Windows",
            0x5c => "Right Windows",
            0x5d => "Application",
            _ => "",
        }
    } else {
        match scan {
            0x01 => "Esc",
            0x0e => "Backspace",
            0x0f => "Tab",
            0x1c => "Enter",
            0x1d => "Ctrl",
            0x2a => "Shift",
            0x36 => "Right Shift",
            0x37 => "Num *",
            0x38 => "Alt",
            0x39 => "Space",
            0x3a => "Caps Lock",
            0x3b..=0x44 => return Some(format!("F{}", scan - 0x3a)),
            0x45 => "Pause",
            0x46 => "Scroll Lock",
            0x47 => "Num 7",
            0x48 => "Num 8",
            0x49 => "Num 9",
            0x4a => "Num -",
            0x4b => "Num 4",
            0x4c => "Num 5",
            0x4d => "Num 6",
            0x4e => "Num +",
            0x4f => "Num 1",
            0x50 => "Num 2",
            0x51 => "Num 3",
            0x52 => "Num 0",
            0x53 => "Num Del",
            0x54 => "Sys Req",
            0x57 => "F11",
            0x58 => "F12",
            0x7c..=0x87 => return Some(format!("F{}", scan - 0x6f)),
            0xf1 if japanese => "ImeOff",
            0xf2 if japanese => "ImeOn",
            _ => "",
        }
    };
    if !label.is_empty() {
        return Some(label.into());
    }
    // Native's unnamed-position fallback uses the unprefixed scan translation,
    // including characters such as extended Esc/Tab and OEM punctuation.
    let character = virtual_key_character(scan_virtual_key(scan, japanese));
    Some(
        char::from_u32(character)
            .filter(|&c| c != '\0')
            .map_or_else(String::new, |c| c.to_string()),
    )
}

fn virtual_key_scan(code: u32, japanese: bool) -> u32 {
    match code {
        0x03 => 0xe046,
        0x0c => 0x4c,
        0x13 => 0xe11d,
        0x16 if japanese => 0xe0f2,
        0x1a if japanese => 0xe0f1,
        // Native picks the unprefixed keypad position for navigation/Print,
        // even when an E0 position can produce the same virtual key.
        0x21 => 0x49,
        0x22 => 0x51,
        0x23 => 0x4f,
        0x24 => 0x47,
        0x25 => 0x4b,
        0x26 => 0x48,
        0x27 => 0x4d,
        0x28 => 0x50,
        0x2c => 0x54,
        0x2d => 0x52,
        0x2e => 0x53,
        0x2f => 0x63,
        0x5f => 0xe05f,
        0x7c..=0x86 => 0x64 + code - 0x7c,
        0x87 => 0x76,
        0xa6 => 0xe06a,
        0xa7 => 0xe069,
        0xa8 => 0xe067,
        0xa9 => 0xe068,
        0xaa => 0xe065,
        0xab => 0xe066,
        0xac => 0xe032,
        0xad => 0xe020,
        0xae => 0xe02e,
        0xaf => 0xe030,
        0xb0 => 0xe019,
        0xb1 => 0xe010,
        0xb2 => 0xe024,
        0xb3 => 0xe022,
        0xb4 => 0xe06c,
        0xb5 => 0xe06d,
        0xb6 => 0xe06b,
        0xb7 => 0xe021,
        0xc1 => 0x73,
        0xc2 => 0x7e,
        0xe2 => 0x56,
        0xe9 => 0x71,
        0xea => 0x5c,
        0xeb => 0x7b,
        0xec if japanese => 0x7f,
        0xed => 0x6f,
        0xee => 0x5a,
        0xf1 => 0x5b,
        0xf3 => 0x5f,
        0xf5 => 0x5e,
        0xf9 => 0x5d,
        0xfb => 0x62,
        _ => {
            let vk = match code {
                0x10 => 0xa0,
                0x11 => 0xa2,
                0x12 => 0xa4,
                _ => code,
            };
            (1_u8..=u8::MAX)
                .find(|&scan| directinput_virtual_key(scan) == Some(vk as usize))
                .map_or(0, |scan| {
                    if scan & 0x80 != 0 {
                        0xe000 | u32::from(scan & 0x7f)
                    } else {
                        u32::from(scan)
                    }
                })
        }
    }
}

fn scan_virtual_key(scan: u32, japanese: bool) -> u32 {
    match scan {
        0x47 => 0x24,
        0x48 => 0x26,
        0x49 => 0x21,
        0x4b => 0x25,
        0x4c | 0x59 => 0x0c,
        0x4d => 0x27,
        0x4f => 0x23,
        0x50 => 0x28,
        0x51 => 0x22,
        0x52 => 0x2d,
        0x53 => 0x2e,
        0x54 => 0x2c,
        0x56 => 0xe2,
        0x5a => 0xee,
        0x5b => 0xf1,
        0x5c => 0xea,
        0x5d => 0xf9,
        0x5e => 0xf5,
        0x5f => 0xf3,
        0x62 => 0xfb,
        0x63 => 0x2f,
        0x64..=0x6e => 0x7c + scan - 0x64,
        0x6f => 0xed,
        0x71 => 0xe9,
        0x73 => 0xc1,
        0x76 => 0x87,
        0x7b => 0xeb,
        0x7c => 0x09,
        0x7e => 0xc2,
        0x7f if japanese => 0xec,
        0xe01c => 0x0d,
        0xe010 => 0xb1,
        0xe019 => 0xb0,
        0xe020 => 0xad,
        0xe021 => 0xb7,
        0xe022 => 0xb3,
        0xe024 => 0xb2,
        0xe02e => 0xae,
        0xe030 => 0xaf,
        0xe032 => 0xac,
        0xe046 => 0x03,
        0xe05f => 0x5f,
        0xe065 => 0xaa,
        0xe066 => 0xab,
        0xe067 => 0xa8,
        0xe068 => 0xa9,
        0xe069 => 0xa7,
        0xe06a => 0xa6,
        0xe06b => 0xb6,
        0xe06c => 0xb4,
        0xe06d => 0xb5,
        0xe0f1 if japanese => 0x1a,
        0xe0f2 if japanese => 0x16,
        0xe11d => 0x13,
        0..=0x7f => directinput_virtual_key(scan as u8).unwrap_or(0) as u32,
        0xe000..=0xe07f if scan != 0xe045 => {
            directinput_virtual_key((scan as u8) | 0x80).unwrap_or(0) as u32
        }
        _ => 0,
    }
}

fn virtual_key_character(code: u32) -> u32 {
    match code {
        0x03 | 0x08 | 0x09 | 0x0d | 0x1b | 0x20 | 0x30..=0x39 | 0x41..=0x5a => code,
        0x60..=0x69 => b'0' as u32 + code - 0x60,
        0x6a => b'*' as u32,
        0x6b => b'+' as u32,
        0x6d | 0xbd => b'-' as u32,
        0x6e | 0xbe => b'.' as u32,
        0x6f | 0xbf => b'/' as u32,
        0xba => b';' as u32,
        0xbb => b'=' as u32,
        0xbc => b',' as u32,
        0xc0 => b'`' as u32,
        0xdb => b'[' as u32,
        0xdc | 0xe2 => b'\\' as u32,
        0xdd => b']' as u32,
        0xde => b'\'' as u32,
        _ => 0,
    }
}
