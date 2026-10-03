//! Code pages: ANSI <-> UTF-16 conversions for the guest's configured code page.

use encoding_rs::{Encoding, GBK, SHIFT_JIS, UTF_8, WINDOWS_1252};

pub const CP_ACP: u32 = 0;
pub const CP_OEMCP: u32 = 1;
pub const CP_THREAD_ACP: u32 = 3;
pub const CP_UTF8: u32 = 65001;

#[derive(Clone, Copy, Debug)]
pub struct Codec {
    /// The ANSI code page (CP_ACP resolves to this).
    pub acp: u32,
}

/// Lead-byte ranges of a DBCS code page.
pub fn lead_byte_ranges(cp: u32) -> &'static [(u8, u8)] {
    match cp {
        932 => &[(0x81, 0x9f), (0xe0, 0xfc)],
        936 => &[(0x81, 0xfe)],
        _ => &[],
    }
}

pub fn is_valid_codepage(cp: u32) -> bool {
    matches!(cp, 932 | 936 | 1252 | 437 | CP_UTF8 | 20127)
}

impl Codec {
    pub fn new(acp: u32) -> Self {
        Self { acp }
    }

    pub fn resolve(&self, cp: u32) -> u32 {
        match cp {
            CP_ACP | CP_OEMCP | CP_THREAD_ACP => self.acp,
            c => c,
        }
    }

    fn encoding(&self, cp: u32) -> &'static Encoding {
        match self.resolve(cp) {
            // WHATWG Shift_JIS is Windows-31J (NEC/IBM extensions included), as is GBK/cp936.
            932 => SHIFT_JIS,
            936 => GBK,
            CP_UTF8 => UTF_8,
            _ => WINDOWS_1252,
        }
    }

    /// Bytes in code page `cp` to UTF-16.
    pub fn to_utf16(&self, cp: u32, bytes: &[u8]) -> Vec<u16> {
        let (s, _) = self.encoding(cp).decode_without_bom_handling(bytes);
        s.encode_utf16().collect()
    }

    /// UTF-16 to code page `cp`; unmappable characters become `?`. Returns whether any did.
    pub fn from_utf16(&self, cp: u32, units: &[u16]) -> (Vec<u8>, bool) {
        let s = String::from_utf16_lossy(units);
        let (b, _, lossy) = self.encoding(cp).encode(&s);
        if !lossy {
            return (b.into_owned(), false);
        }
        // encoding_rs substitutes HTML numeric references; Windows uses the default char.
        let mut out = Vec::with_capacity(b.len());
        let mut buf = [0u8; 4];
        for ch in s.chars() {
            let (e, _, bad) = self.encoding(cp).encode(ch.encode_utf8(&mut buf));
            if bad {
                out.push(b'?')
            } else {
                out.extend_from_slice(&e)
            }
        }
        (out, true)
    }

    /// Guest ANSI bytes (ACP) to a Rust string, for paths and logs.
    pub fn decode(&self, bytes: &[u8]) -> String {
        self.encoding(CP_ACP)
            .decode_without_bom_handling(bytes)
            .0
            .into_owned()
    }

    /// Rust string to guest ANSI bytes (ACP).
    pub fn encode(&self, s: &str) -> Vec<u8> {
        let units: Vec<u16> = s.encode_utf16().collect();
        self.from_utf16(CP_ACP, &units).0
    }

    pub fn is_lead_byte(&self, cp: u32, b: u8) -> bool {
        lead_byte_ranges(self.resolve(cp))
            .iter()
            .any(|&(lo, hi)| (lo..=hi).contains(&b))
    }
}

/// CT_CTYPE1 classification of a UTF-16 unit.
pub fn ctype1(c: u16) -> u16 {
    const UPPER: u16 = 0x1;
    const LOWER: u16 = 0x2;
    const DIGIT: u16 = 0x4;
    const SPACE: u16 = 0x8;
    const PUNCT: u16 = 0x10;
    const CNTRL: u16 = 0x20;
    const BLANK: u16 = 0x40;
    const XDIGIT: u16 = 0x80;
    const ALPHA: u16 = 0x100;
    const DEFINED: u16 = 0x200;
    let Some(ch) = char::from_u32(u32::from(c)) else {
        return DEFINED;
    };
    let mut t = DEFINED;
    if ch.is_uppercase() {
        t |= UPPER | ALPHA;
    }
    if ch.is_lowercase() {
        t |= LOWER | ALPHA;
    }
    if ch.is_alphabetic() {
        t |= ALPHA;
    }
    if ch.is_ascii_digit() {
        t |= DIGIT;
    }
    if ch.is_ascii_hexdigit() {
        t |= XDIGIT;
    }
    if ch.is_whitespace() {
        t |= SPACE;
    }
    if ch == ' ' || ch == '\t' || ch == '\u{3000}' {
        t |= BLANK;
    }
    if ch.is_control() {
        t |= CNTRL;
    }
    if ch.is_ascii_punctuation()
        || (!ch.is_alphanumeric() && !ch.is_whitespace() && !ch.is_control() && c > 0x7f)
    {
        t |= PUNCT;
    }
    t
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sjis_round_trip_and_default_char() {
        let c = Codec::new(932);
        let sjis = [0x82, 0xa0, 0x41]; // あA
        let w = c.to_utf16(CP_ACP, &sjis);
        assert_eq!(w, [0x3042, 0x41]);
        assert_eq!(c.from_utf16(932, &w), (sjis.to_vec(), false));
        // U+00E9 has no Shift_JIS mapping.
        assert_eq!(c.from_utf16(932, &[0xe9, 0x41]), (b"?A".to_vec(), true));
        assert!(c.is_lead_byte(CP_ACP, 0x82) && !c.is_lead_byte(CP_ACP, 0x41));
    }

    #[test]
    fn gbk() {
        let c = Codec::new(936);
        assert_eq!(c.decode(&[0xd6, 0xd0]), "中");
        assert_eq!(c.encode("中"), [0xd6, 0xd0]);
    }
}
