//! Default ANSI code page for a game executable. Simplified Chinese builds are
//! recognized by a name marker such as `_chs`, or by non-executable sections
//! holding clearly more GB2312 text than Shift_JIS text. Everything else runs
//! as Japanese. Text that lives only in archives is not visible here.
use std::collections::BTreeSet;

const JAPANESE: u32 = 932;
const SIMPLIFIED_CHINESE: u32 = 936;
const NAME_MARKERS: [&str; 4] = ["chs", "cn", "gb", "zhcn"];
const MIN_CHINESE: usize = 16;
const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;

/// Code page for the executable `name` with contents `image`.
pub fn detect(name: &str, image: &[u8]) -> u32 {
    let stem = name.rsplit_once('.').map_or(name, |(stem, _)| stem);
    if stem
        .to_ascii_lowercase()
        .split(['_', '-', '.', ' '])
        .any(|token| NAME_MARKERS.contains(&token))
    {
        return SIMPLIFIED_CHINESE;
    }
    let (japanese, chinese) = text_evidence(image);
    if chinese >= MIN_CHINESE && chinese >= 3 * japanese {
        SIMPLIFIED_CHINESE
    } else {
        JAPANESE
    }
}

/// Distinct common Shift_JIS and GB2312 characters in NUL-terminated strings
/// of the image's data sections. Padding and code bytes rarely form three
/// distinct common characters in a row, so such strings are ignored.
fn text_evidence(image: &[u8]) -> (usize, usize) {
    let Ok(pe) = adv_pe::Pe::parse_headers(image) else {
        return (0, 0);
    };
    let (mut japanese, mut chinese) = (BTreeSet::new(), BTreeSet::new());
    for section in &pe.sections {
        if section.characteristics & IMAGE_SCN_MEM_EXECUTE != 0 {
            continue;
        }
        let start = section.raw_offset as usize;
        let end = start.saturating_add(section.raw_size as usize);
        let Some(data) = image.get(start..end.min(image.len())) else {
            continue;
        };
        for text in data.split(|&byte| byte == 0) {
            if text.iter().filter(|&&byte| byte >= 0x80).count() < 4
                || text
                    .iter()
                    .any(|&byte| byte == 0x7f || (byte < 0x20 && !b"\t\n\r".contains(&byte)))
            {
                continue;
            }
            for (classify, found) in [
                (shift_jis as fn(&[u8]) -> Option<Score>, &mut japanese),
                (gbk, &mut chinese),
            ] {
                if let Some(score) = classify(text)
                    && score.common.len() >= 3
                    && score.common.len() >= 4 * score.rare
                {
                    found.extend(score.common);
                }
            }
        }
    }
    (japanese.len(), chinese.len())
}

struct Score {
    common: BTreeSet<[u8; 2]>,
    /// Characters the other encoding's text produces when misread as this one.
    rare: usize,
}

/// Kana, punctuation and JIS level 1 kanji are common; half-width katakana and
/// user-defined characters are rare. Invalid sequences reject the string.
fn shift_jis(text: &[u8]) -> Option<Score> {
    let mut score = Score {
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
fn gbk(text: &[u8]) -> Option<Score> {
    let mut score = Score {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal PE image with one data section holding `data`.
    fn image(data: &[u8]) -> Vec<u8> {
        let mut image = vec![0; 0x400];
        image[..2].copy_from_slice(b"MZ");
        image[0x3c..0x40].copy_from_slice(&0x80u32.to_le_bytes());
        image[0x80..0x84].copy_from_slice(b"PE\0\0");
        image[0x84..0x86].copy_from_slice(&0x14cu16.to_le_bytes());
        image[0x86..0x88].copy_from_slice(&1u16.to_le_bytes());
        image[0x94..0x96].copy_from_slice(&0xe0u16.to_le_bytes());
        image[0x96..0x98].copy_from_slice(&0x0102u16.to_le_bytes());
        let optional = 0x98;
        image[optional..optional + 2].copy_from_slice(&0x10bu16.to_le_bytes());
        image[optional + 28..optional + 32].copy_from_slice(&0x40_0000u32.to_le_bytes());
        image[optional + 32..optional + 36].copy_from_slice(&0x1000u32.to_le_bytes());
        image[optional + 36..optional + 40].copy_from_slice(&0x200u32.to_le_bytes());
        image[optional + 56..optional + 60].copy_from_slice(&0x2000u32.to_le_bytes());
        image[optional + 60..optional + 64].copy_from_slice(&0x200u32.to_le_bytes());
        image[optional + 92..optional + 96].copy_from_slice(&16u32.to_le_bytes());
        let section = optional + 0xe0;
        image[section..section + 5].copy_from_slice(b".data");
        image[section + 8..section + 12].copy_from_slice(&0x1000u32.to_le_bytes());
        image[section + 12..section + 16].copy_from_slice(&0x1000u32.to_le_bytes());
        image[section + 16..section + 20].copy_from_slice(&0x200u32.to_le_bytes());
        image[section + 20..section + 24].copy_from_slice(&0x200u32.to_le_bytes());
        image[section + 36..section + 40].copy_from_slice(&0xc000_0040u32.to_le_bytes());
        image[0x200..0x200 + data.len()].copy_from_slice(data);
        image
    }

    fn strings(encoding: &'static encoding_rs::Encoding, texts: &[&str]) -> Vec<u8> {
        let mut data = vec![0];
        for text in texts {
            data.extend_from_slice(&encoding.encode(text).0);
            data.push(0);
        }
        data
    }

    #[test]
    // spec: PERS-PROFILE-7
    fn executables_default_to_japanese_unless_marked_or_mostly_gb2312() {
        let japanese = strings(
            encoding_rs::SHIFT_JIS,
            &[
                "セーブデータを読み込みますか？",
                "ゲームを終了します。よろしいですか？",
            ],
        );
        let chinese = strings(
            encoding_rs::GBK,
            &[
                "是否读取存档？",
                "确定要结束游戏吗？",
                "返回标题画面",
                "文字显示速度",
            ],
        );
        let filler = image(&[0xcc; 64]);
        assert_eq!(detect("game.exe", &image(&japanese)), 932);
        assert_eq!(detect("game.exe", &image(&chinese)), 936);
        assert_eq!(detect("game.exe", &filler), 932);
        assert_eq!(detect("game.exe", b"not a PE image"), 932);
        // A translated build keeps most of its original strings.
        assert_eq!(detect("Game_chs.exe", &image(&japanese)), 936);
        assert_eq!(detect("nana-CN.exe", &[]), 936);
        assert_eq!(detect("chaos.exe", &[]), 932);
    }
}
