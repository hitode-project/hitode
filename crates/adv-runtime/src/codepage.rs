//! Default ANSI code page for a game executable. Simplified Chinese builds are
//! recognized by a name marker such as `_chs`, a name written with simplified
//! characters that Shift_JIS lacks, or by non-executable sections
//! holding clearly more GB2312 text than Shift_JIS text. Everything else runs
//! as Japanese. Text that lives only in archives is not visible here.
use adv_os::nls;
use std::collections::BTreeSet;
use std::io::Read;
use std::path::Path;

const JAPANESE: u32 = 932;
const SIMPLIFIED_CHINESE: u32 = 936;
const NAME_MARKERS: [&str; 4] = ["chs", "cn", "gb", "zhcn"];
const MIN_CHINESE: usize = 16;
const IMAGE_SCN_MEM_EXECUTE: u32 = 0x2000_0000;

/// Longest header block read before the section table is known.
const HEADERS: u64 = 64 * 1024;

/// Code page for the executable at `path`, reading only its headers and
/// sections: some engines append all their assets, often gigabytes, after
/// the image.
pub fn detect_file(path: &Path) -> std::io::Result<u32> {
    let mut file = std::fs::File::open(path)?;
    let mut image = Vec::new();
    (&mut file).take(HEADERS).read_to_end(&mut image)?;
    let end = adv_pe::Pe::parse_headers(&image).map_or(0, |pe| {
        pe.sections
            .iter()
            .map(|s| u64::from(s.raw_offset) + u64::from(s.raw_size))
            .max()
            .unwrap_or(0)
    });
    if let Some(rest) = end.checked_sub(image.len() as u64) {
        file.take(rest).read_to_end(&mut image)?;
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    Ok(detect(&name, &image))
}

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
    if simplified_name(stem) {
        return SIMPLIFIED_CHINESE;
    }
    let (japanese, chinese) = text_evidence(image);
    if chinese >= MIN_CHINESE && chinese >= 3 * japanese {
        SIMPLIFIED_CHINESE
    } else {
        JAPANESE
    }
}

/// A kana-free name with a CJK character that GBK has and Shift_JIS lacks,
/// such as 时 or 线, is a Simplified Chinese title.
fn simplified_name(stem: &str) -> bool {
    let codec = nls::Codec::new(JAPANESE);
    let mut units = [0; 2];
    !stem.chars().any(|c| matches!(c, '\u{3040}'..='\u{30ff}'))
        && stem.chars().any(|c| {
            let units = c.encode_utf16(&mut units);
            matches!(c, '\u{4e00}'..='\u{9fff}')
                && codec.from_utf16(JAPANESE, units).1
                && !codec.from_utf16(SIMPLIFIED_CHINESE, units).1
        })
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
            for (codepage, found) in [
                (JAPANESE, &mut japanese),
                (SIMPLIFIED_CHINESE, &mut chinese),
            ] {
                if let Some(score) = nls::text_score(codepage, text)
                    && score.is_text(3)
                {
                    found.extend(score.common);
                }
            }
        }
    }
    (japanese.len(), chinese.len())
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
    fn file_detection_reads_the_sections_and_skips_appended_data() {
        let chinese = strings(
            encoding_rs::GBK,
            &[
                "是否读取存档数据？",
                "确定要结束游戏吗？",
                "请选择一个存档位置",
            ],
        );
        let mut bytes = image(&chinese);
        // Appended Japanese text outweighs the sections, but is not read.
        let appended = strings(
            encoding_rs::SHIFT_JIS,
            &["セーブデータを読み込みますか？"; 8],
        );
        for _ in 0..64 {
            bytes.extend_from_slice(&appended);
        }
        let path = std::env::temp_dir().join(format!("adv-codepage-{}.exe", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(
            detect_file(&path).unwrap(),
            detect("game.exe", &image(&chinese))
        );
        assert_eq!(detect_file(&path).unwrap(), SIMPLIFIED_CHINESE);
        std::fs::remove_file(path).unwrap();
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
        // Simplified characters missing from Shift_JIS name a Chinese build.
        assert_eq!(detect("黄昏时的境界线.exe", &image(&japanese)), 936);
        assert_eq!(detect("黄昏時の境界線.exe", &[]), 932);
        assert_eq!(detect("白詰草話.exe", &[]), 932);
    }
}
