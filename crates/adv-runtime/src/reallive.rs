//! RealLive configuration as a GBK conversion plugin expects it.
//!
//! Simplified Chinese translations of RealLive games ship GBK scenarios and a
//! plugin, `rlgbk`, that switches the engine's text handling to GBK when it
//! loads. The engine parses `Gameexe.ini` before loading any plugin, with
//! Shift_JIS lead bytes, so translations keep that file in Shift_JIS. Its
//! default character names (`#NAME.*`) then reach the GBK scenario as
//! Shift_JIS bytes: dialogue shows them garbled, and scenario comparisons
//! against the translated names fail and skip their voices. Windows players
//! repair this once through the game's own name menu.
//!
//! The session instead shows such a game a copy of the file whose name values
//! are the simplified GBK forms the scenario uses (PERS-PROFILE-10). Only
//! complete values that both code pages hold are converted, and a value whose
//! last GBK byte the Shift_JIS parser would read as a lead byte ends in a NUL,
//! which the engine copies as the string's end, so the closing quote survives.
use std::path::{Path, PathBuf};

use adv_os::nls::{Codec, lead_byte_ranges};

mod simplified;

const JAPANESE: u32 = 932;
const SIMPLIFIED_CHINESE: u32 = 936;
const PLUGIN: &[u8] = b"rlgbk";

/// The game's `Gameexe.ini` and the text its plugin expects, when they differ.
pub fn configuration(game: &Path) -> Option<(PathBuf, Vec<u8>)> {
    let path = std::fs::read_dir(game).ok()?.flatten().find_map(|entry| {
        let name = entry.file_name();
        name.eq_ignore_ascii_case("gameexe.ini")
            .then(|| entry.path())
    })?;
    let corrected = correct(&std::fs::read(&path).ok()?)?;
    Some((path, corrected))
}

fn correct(text: &[u8]) -> Option<Vec<u8>> {
    let lines: Vec<&[u8]> = text.split_inclusive(|&byte| byte == b'\n').collect();
    let uses_plugin = lines.iter().any(|line| {
        line.starts_with(b"#DLL.")
            && value(line).is_some_and(|(start, end)| line[start..end].eq_ignore_ascii_case(PLUGIN))
    });
    if !uses_plugin {
        return None;
    }
    let mut corrected = Vec::with_capacity(text.len() + 8);
    let mut changed = false;
    for line in lines {
        let converted = line
            .starts_with(b"#NAME.")
            .then(|| value(line))
            .flatten()
            .and_then(|(start, end)| Some((start, end, convert(&line[start..end])?)));
        match converted {
            Some((start, end, name)) => {
                corrected.extend_from_slice(&line[..start]);
                corrected.extend_from_slice(&name);
                corrected.extend_from_slice(&line[end..]);
                changed = true;
            }
            None => corrected.extend_from_slice(line),
        }
    }
    changed.then_some(corrected)
}

/// The quoted value after `=`, delimited as the Shift_JIS parser reads it.
fn value(line: &[u8]) -> Option<(usize, usize)> {
    let equals = line.iter().position(|&byte| byte == b'=')?;
    let quote = equals + 1 + line[equals + 1..].iter().position(|&byte| byte != b' ')?;
    if line[quote] != b'"' {
        return None;
    }
    let start = quote + 1;
    let mut at = start;
    while at < line.len() {
        if line[at] == b'"' {
            return Some((start, at));
        }
        at += if is_lead(line[at]) { 2 } else { 1 };
    }
    None
}

fn is_lead(byte: u8) -> bool {
    lead_byte_ranges(JAPANESE)
        .iter()
        .any(|&(low, high)| (low..=high).contains(&byte))
}

fn convert(name: &[u8]) -> Option<Vec<u8>> {
    let codec = Codec::new(JAPANESE);
    let (units, replaced) = codec.to_utf16_checked(JAPANESE, name);
    if replaced {
        return None;
    }
    let units: Vec<u16> = units.into_iter().map(simplify).collect();
    let (mut converted, lossy) = codec.from_utf16(SIMPLIFIED_CHINESE, &units);
    if lossy || converted == name {
        return None;
    }
    let mut at = 0;
    while at < converted.len() {
        at += if is_lead(converted[at]) { 2 } else { 1 };
    }
    if at > converted.len() {
        converted.push(0);
    }
    Some(converted)
}

fn simplify(unit: u16) -> u16 {
    simplified::SIMPLIFIED
        .binary_search_by_key(&unit, |&(japanese, _)| japanese)
        .map_or(unit, |index| simplified::SIMPLIFIED[index].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sjis(text: &str) -> Vec<u8> {
        Codec::new(JAPANESE)
            .from_utf16(JAPANESE, &text.encode_utf16().collect::<Vec<_>>())
            .0
    }
    fn gbk(text: &str) -> Vec<u8> {
        Codec::new(SIMPLIFIED_CHINESE)
            .from_utf16(SIMPLIFIED_CHINESE, &text.encode_utf16().collect::<Vec<_>>())
            .0
    }
    fn ini(names: [&str; 2], plugin: &str) -> Vec<u8> {
        let mut text = b"#DLL.000=\"".to_vec();
        text.extend_from_slice(plugin.as_bytes());
        text.extend_from_slice(b"\"\r\n#NAME_MAXLEN=10\r\n#NAME.A=\"");
        text.extend_from_slice(&sjis(names[0]));
        text.extend_from_slice(b"\"\r\n#NAME.B=\"");
        text.extend_from_slice(&sjis(names[1]));
        text.extend_from_slice(b"\"\r\n#WINDOW.000.NAME_MOD=0\r\n");
        text
    }

    // spec: PERS-PROFILE-10
    #[test]
    fn plugin_names_become_simplified_gbk_that_shift_jis_still_delimits() {
        let corrected = correct(&ini(["岡崎", "朋也"], "RLGBK")).expect("names change");
        let mut expected = b"#DLL.000=\"RLGBK\"\r\n#NAME_MAXLEN=10\r\n#NAME.A=\"".to_vec();
        // 崎 ends in 0xE9, a Shift_JIS lead byte that would take the quote.
        expected.extend_from_slice(&gbk("冈崎"));
        expected.extend_from_slice(b"\0\"\r\n#NAME.B=\"");
        expected.extend_from_slice(&gbk("朋也"));
        expected.extend_from_slice(b"\"\r\n#WINDOW.000.NAME_MOD=0\r\n");
        assert_eq!(corrected, expected);
        for line in corrected
            .split_inclusive(|&byte| byte == b'\n')
            .skip(2)
            .take(2)
        {
            let (start, end) = value(line).expect("the parser finds the closing quote");
            assert_eq!(line[end + 1..].to_vec(), b"\r\n");
            assert!(start < end);
        }
    }

    // spec: PERS-PROFILE-10
    #[test]
    fn configurations_without_the_plugin_or_a_changed_name_stay() {
        assert_eq!(correct(&ini(["岡崎", "朋也"], "other")), None);
        assert_eq!(correct(&ini(["Tom", "Ya"], "rlgbk")), None);
    }

    // spec: PERS-PROFILE-10
    #[test]
    fn japanese_forms_become_simplified_and_shared_forms_stay() {
        assert_eq!(
            convert(&sjis("桜沢")),
            Some([gbk("樱泽"), vec![0]].concat())
        );
        assert_eq!(convert(&sjis("予台")), Some(gbk("予台")));
        assert_eq!(convert(&sjis("古河")), Some(gbk("古河")));
    }
}
