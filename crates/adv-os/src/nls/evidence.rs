//! How plausible guest DBCS bytes are as Japanese or Simplified Chinese text.
//! The common ranges of one encoding are mostly rare or invalid in the other,
//! so the two readings of a string rarely both look like ordinary text.
use std::collections::BTreeSet;

pub struct TextScore {
    /// Distinct common characters, as their two bytes.
    pub common: BTreeSet<[u8; 2]>,
    /// Characters the other encoding's text produces when misread as this one.
    pub rare: usize,
}

impl TextScore {
    /// Ordinary text: a few distinct common characters and few rare ones.
    pub fn is_text(&self, min_common: usize) -> bool {
        self.common.len() >= min_common && self.common.len() >= 4 * self.rare
    }
}

/// Score `text` as code page 932 or 936. Invalid sequences give `None`, as
/// does any other code page.
pub fn text_score(codepage: u32, text: &[u8]) -> Option<TextScore> {
    match codepage {
        932 => shift_jis(text),
        936 => gbk(text),
        _ => None,
    }
}

/// Kana, punctuation and JIS level 1 kanji are common; half-width katakana and
/// user-defined characters are rare.
fn shift_jis(text: &[u8]) -> Option<TextScore> {
    let mut score = TextScore {
        common: BTreeSet::new(),
        rare: 0,
    };
    let mut at = 0;
    while let Some(&lead) = text.get(at) {
        if lead < 0x80 {
            at += 1;
            continue;
        }
        if (0xa1..=0xdf).contains(&lead) {
            score.rare += 1;
            at += 1;
            continue;
        }
        let trail = *text.get(at + 1)?;
        if !matches!(lead, 0x81..=0x9f | 0xe0..=0xfc) || !matches!(trail, 0x40..=0x7e | 0x80..=0xfc)
        {
            return None;
        }
        let common = match lead {
            0x81 => trail <= 0xac,
            0x82 => (0x4f..=0xf1).contains(&trail),
            0x83 => trail <= 0x96,
            0x88 => trail >= 0x9f,
            0x89..=0x97 => true,
            0x98 => trail <= 0x72,
            _ => false,
        };
        if common {
            score.common.insert([lead, trail]);
        } else if (0xf0..=0xf9).contains(&lead) {
            score.rare += 1;
        }
        at += 2;
    }
    Some(score)
}

/// GB2312 punctuation, full-width ASCII and level 1 hanzi are common; the GBK
/// extension areas and user-defined rows are rare.
fn gbk(text: &[u8]) -> Option<TextScore> {
    let mut score = TextScore {
        common: BTreeSet::new(),
        rare: 0,
    };
    let mut at = 0;
    while let Some(&lead) = text.get(at) {
        if lead < 0x80 {
            at += 1;
            continue;
        }
        let trail = *text.get(at + 1)?;
        if !(0x81..=0xfe).contains(&lead) || !(0x40..=0xfe).contains(&trail) || trail == 0x7f {
            return None;
        }
        if trail >= 0xa1 && matches!(lead, 0xa1 | 0xa3 | 0xb0..=0xd7) {
            score.common.insert([lead, trail]);
        } else if lead <= 0xa0 || trail <= 0xa0 || lead >= 0xf8 {
            score.rare += 1;
        }
        at += 2;
    }
    Some(score)
}
