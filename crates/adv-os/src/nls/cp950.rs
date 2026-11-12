//! Windows code page 950 where it departs from WHATWG Big5: the user-defined
//! (EUDC) rows map linearly onto private use instead of HKSCS, a few cells
//! differ, and encoding is the inverse of that decoding.

use encoding_rs::BIG5;

use super::single;

/// Each EUDC area: first lead, last lead, first trail of the first row and
/// the private-use unit of that first cell. Together they cover U+E000..U+F848.
const EUDC: [(u8, u8, u8, u16); 4] = [
    (0xfa, 0xfe, 0x40, 0xe000),
    (0x8e, 0xa0, 0x40, 0xe311),
    (0x81, 0x8d, 0x40, 0xeeb8),
    (0xc6, 0xc8, 0xa1, 0xf6b1),
];
const ROW: u16 = 157;
/// Characters whose WHATWG bytes are an HKSCS duplicate of a Windows cell.
const CELLS: [(u32, [u8; 2]); 2] = [(0x2593, [0xf9, 0xfe]), (0x4edd, [0xc9, 0x69])];

/// A trail byte's cell in its 157-cell row.
fn cell(trail: u8) -> Option<u16> {
    match trail {
        0x40..=0x7e => Some(u16::from(trail - 0x40)),
        0xa1..=0xfe => Some(u16::from(trail - 0xa1) + 63),
        _ => None,
    }
}

fn trail(cell: u16) -> u8 {
    if cell < 63 {
        0x40 + cell as u8
    } else {
        0xa1 + (cell - 63) as u8
    }
}

/// The unit of one lead/trail pair; `None` is the default character.
pub(super) fn decode(lead: u8, second: u8) -> Option<u16> {
    let at = cell(second)?;
    for (first, last, start, unit) in EUDC {
        let start = cell(start).unwrap();
        if (first..=last).contains(&lead) && (lead > first || at >= start) {
            return Some(unit + u16::from(lead - first) * ROW + at - start);
        }
    }
    match (lead, second) {
        (0xa3, 0xc0..=0xe0) => None,
        (0xf9, 0xfe) => Some(0x2593),
        _ => single(BIG5, &[lead, second]),
    }
}

/// The bytes of one character, or `None` when it has no mapping.
pub(super) fn encode(ch: char, out: &mut Vec<u8>) -> bool {
    let unit = u32::from(ch);
    if unit < 0x80 {
        out.push(unit as u8);
        return true;
    }
    if unit == 0x80 || unit == 0xf8f8 {
        out.push(if unit == 0x80 { 0x80 } else { 0xff });
        return true;
    }
    for (first, last, start, base) in EUDC {
        let start = cell(start).unwrap();
        let end = u32::from(base) + u32::from(last - first + 1) * u32::from(ROW) - u32::from(start);
        if (u32::from(base)..end).contains(&unit) {
            let offset = (unit - u32::from(base)) as u16 + start;
            out.extend([first + (offset / ROW) as u8, trail(offset % ROW)]);
            return true;
        }
    }
    if let Some((_, bytes)) = CELLS.iter().find(|(cell, _)| *cell == unit) {
        out.extend(bytes);
        return true;
    }
    let mut buf = [0; 4];
    let (bytes, _, unmappable) = BIG5.encode(ch.encode_utf8(&mut buf));
    match *bytes {
        [lead, second] if !unmappable && decode(lead, second) == Some(unit as u16) => {
            out.extend([lead, second]);
            true
        }
        _ => false,
    }
}
