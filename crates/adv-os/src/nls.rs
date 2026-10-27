//! Code pages: ANSI <-> UTF-16 conversions for the guest's configured code page.

use encoding_rs::{Encoding, GBK, SHIFT_JIS, UTF_8, WINDOWS_1252};

mod case_table;
mod evidence;
pub use evidence::{TextScore, text_score};

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

/// The conversion table of a code page converted as Windows does.
fn table(cp: u32) -> Option<&'static Encoding> {
    match cp {
        // WHATWG Shift_JIS is Windows-31J (NEC/IBM extensions included), as is GBK/cp936.
        932 => Some(SHIFT_JIS),
        936 => Some(GBK),
        CP_UTF8 => Some(UTF_8),
        1252 => Some(WINDOWS_1252),
        _ => None,
    }
}

/// Whether conversions in `cp` match Windows: the ANSI aliases and the code
/// pages with a table. Any other code page converts as Windows-1252.
pub fn is_modeled(cp: u32) -> bool {
    matches!(cp, CP_ACP | CP_OEMCP | CP_THREAD_ACP) || table(cp).is_some()
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

    /// Code pages without a table convert as Windows-1252.
    fn encoding(&self, cp: u32) -> &'static Encoding {
        table(self.resolve(cp)).unwrap_or(WINDOWS_1252)
    }

    /// Bytes in code page `cp` to UTF-16, as MultiByteToWideChar without
    /// MB_ERR_INVALID_CHARS converts them.
    pub fn to_utf16(&self, cp: u32, bytes: &[u8]) -> Vec<u16> {
        self.to_utf16_checked(cp, bytes).0
    }

    /// [`Self::to_utf16`], also returning whether any sequence was invalid
    /// and became the code page's default character.
    pub fn to_utf16_checked(&self, cp: u32, bytes: &[u8]) -> (Vec<u16>, bool) {
        let cp = self.resolve(cp);
        let Some(default) = dbcs_default(cp) else {
            let (s, replaced) = self.encoding(cp).decode_without_bom_handling(bytes);
            return (s.encode_utf16().collect(), replaced);
        };
        // WHATWG Shift_JIS and GBK differ from the Windows tables at their
        // edges: a lead byte always consumes the next byte, an undefined pair
        // or a lone final lead byte is one default character, a few single
        // bytes map to private use, and cp936 has no GB18030 four-byte form.
        let encoding = self.encoding(cp);
        let (mut units, mut replaced) = (Vec::with_capacity(bytes.len()), false);
        let mut i = 0;
        while i < bytes.len() {
            let byte = bytes[i];
            if byte < 0x80 {
                units.push(u16::from(byte));
                i += 1;
                continue;
            }
            if !self.is_lead_byte(cp, byte) {
                units.push(match (cp, byte) {
                    (932, 0xa0) => 0xf8f0,
                    (932, 0xfd..=0xff) => 0xf8f1 + u16::from(byte - 0xfd),
                    (936, 0xff) => 0xf8f5,
                    _ => single(encoding, &[byte]).unwrap_or(default),
                });
                i += 1;
                continue;
            }
            let decoded = bytes.get(i..i + 2).and_then(|pair| single(encoding, pair));
            replaced |= decoded.is_none();
            units.push(decoded.unwrap_or(default));
            i += 2;
        }
        (units, replaced)
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
        String::from_utf16_lossy(&self.to_utf16(CP_ACP, bytes))
    }

    /// Guest ANSI bytes the host only displays. A translation patch can leave
    /// a string in the other DBCS encoding: when the ACP reading is not
    /// ordinary text and the 932/936 counterpart is, show the counterpart.
    pub fn decode_for_display(&self, bytes: &[u8]) -> String {
        let other = match self.acp {
            932 => 936,
            936 => 932,
            _ => return self.decode(bytes),
        };
        let is_text = |codepage| text_score(codepage, bytes).is_some_and(|s| s.is_text(2));
        if !is_text(self.acp) && is_text(other) {
            String::from_utf16_lossy(&self.to_utf16(other, bytes))
        } else {
            self.decode(bytes)
        }
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

/// The character MultiByteToWideChar substitutes for an invalid sequence in
/// a DBCS code page.
fn dbcs_default(cp: u32) -> Option<u16> {
    match cp {
        932 => Some(0x30fb),
        936 => Some(u16::from(b'?')),
        _ => None,
    }
}

/// One complete sequence that decodes to exactly one BMP character.
fn single(encoding: &'static Encoding, bytes: &[u8]) -> Option<u16> {
    let mut out = [0; 4];
    let (result, read, written) = encoding
        .new_decoder_without_bom_handling()
        .decode_to_utf16_without_replacement(bytes, &mut out, true);
    (matches!(result, encoding_rs::DecoderResult::InputEmpty)
        && read == bytes.len()
        && written == 1)
        .then_some(out[0])
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

/// USER32's invariant case mapping of one UTF-16 unit (CharUpperW, CompareString
/// with NORM_IGNORECASE, registry and atom names).
pub fn unit_case(unit: u16, upper: bool) -> u16 {
    if unit < 0x80 {
        return if upper {
            (unit as u8).to_ascii_uppercase()
        } else {
            (unit as u8).to_ascii_lowercase()
        } as u16;
    }
    // Pin USER32's legacy invariant mapping instead of inheriting Rust's full
    // Unicode expansion rules or changing behavior with the Rust toolchain.
    let table = if upper {
        case_table::UPPER
    } else {
        case_table::LOWER
    };
    let i = table.partition_point(|&(_, last, _, _)| last < unit);
    match table.get(i) {
        Some(&(first, _, stride, delta))
            if unit >= first && (unit - first).is_multiple_of(stride) =>
        {
            (i32::from(unit) + delta) as u16
        }
        _ => unit,
    }
}

#[path = "nls/char_next_table.rs"]
mod char_next_table;

/// Units skipped after the first unit by legacy USER32 CharNextW. This includes
/// leading surrogates; it is neither Unicode grapheme nor scalar segmentation.
pub fn char_next_skips(unit: u16) -> bool {
    let rows = char_next_table::RANGES;
    let index = rows.partition_point(|&(_, last)| last < unit);
    rows.get(index).is_some_and(|&(first, _)| first <= unit)
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
    fn invalid_dbcs_sequences_follow_multi_byte_to_wide_char() {
        // Native MultiByteToWideChar(cp, 0, ...) results.
        let sjis = Codec::new(932);
        for (bytes, units) in [
            (&b"\x82"[..], &[0x30fb][..]),
            (b"A\x82", &[0x41, 0x30fb]),
            (b"\x82A", &[0x30fb]),
            (b"\x82\x7f", &[0x30fb]),
            (b"\x85\x40", &[0x30fb]),
            (b"\x80", &[0x80]),
            (b"\xa0\xfd\xfe\xff", &[0xf8f0, 0xf8f1, 0xf8f2, 0xf8f3]),
            (b"\xf0\x40", &[0xe000]),
            (b"\xfa\x40", &[0x2170]),
            (b"\xa1\x40", &[0xff61, 0x40]),
        ] {
            assert_eq!(sjis.to_utf16(CP_ACP, bytes), units, "932 {bytes:x?}");
        }
        let gbk = Codec::new(936);
        for (bytes, units) in [
            (&b"\x82"[..], &[0x3f][..]),
            (b"\x82A", &[0x4fab]),
            (b"\x82\x7f", &[0x3f]),
            (b"\x82\x20", &[0x3f]),
            (b"\x81\x30\x81\x30", &[0x3f, 0x3f]),
            (b"\x80", &[0x20ac]),
            (b"\xa0", &[0x3f]),
            (b"\xff", &[0xf8f5]),
            (b"\xaa\xa1", &[0xe000]),
        ] {
            assert_eq!(gbk.to_utf16(CP_ACP, bytes), units, "936 {bytes:x?}");
        }
        assert_eq!(gbk.to_utf16_checked(CP_ACP, b"\x82"), (vec![0x3f], true));
        assert_eq!(gbk.to_utf16_checked(CP_ACP, b"?"), (vec![0x3f], false));
    }

    #[test]
    // spec: WIN32-USER-15
    fn displayed_text_falls_back_to_the_other_dbcs_encoding() {
        let japanese = "タイトル画面に戻る(&T)";
        let chinese = "确定要结束游戏吗？";
        for acp in [932, 936] {
            let codec = Codec::new(acp);
            for text in [japanese, chinese, "Quit?"] {
                let bytes = if text == japanese {
                    Codec::new(932).encode(text)
                } else {
                    Codec::new(936).encode(text)
                };
                assert_eq!(codec.decode_for_display(&bytes), text, "{acp} {text}");
            }
        }
        // Text that reads as neither keeps the ACP reading.
        let sjis = Codec::new(932);
        assert_eq!(
            sjis.decode_for_display(b"\xb1\xb2"),
            sjis.decode(b"\xb1\xb2")
        );
    }

    #[test]
    fn gbk() {
        let c = Codec::new(936);
        assert_eq!(c.decode(&[0xd6, 0xd0]), "中");
        assert_eq!(c.encode("中"), [0xd6, 0xd0]);
    }
}
