//! Win32 search expressions over UTF-16 file names, without recursive backtracking.

const STAR: u16 = b'*' as u16;
const QM: u16 = b'?' as u16;
const DOT: u16 = b'.' as u16;
const DOS_STAR: u16 = b'<' as u16;
const DOS_QM: u16 = b'>' as u16;
const DOS_DOT: u16 = b'"' as u16;

fn upcase(unit: u16) -> u16 {
    let Some(character) = char::from_u32(u32::from(unit)) else {
        // Surrogates remain separate code units, including supplementary letters.
        return unit;
    };
    let mut uppercase = character.to_uppercase();
    let first = uppercase.next().unwrap();
    if uppercase.next().is_none() && first.len_utf16() == 1 {
        first as u16
    } else {
        // A filesystem case table maps one unit to one unit, never to "SS", etc.
        unit
    }
}

fn expression(pattern: &str) -> Vec<u16> {
    let units: Vec<_> = pattern.encode_utf16().collect();
    let mut result = Vec::with_capacity(units.len());
    for (index, &unit) in units.iter().enumerate() {
        match unit {
            QM => result.push(DOS_QM),
            STAR if units.get(index + 1) == Some(&DOT) => result.push(DOS_STAR),
            DOT if index + 1 == units.len() && index > 0 && units[index - 1] == STAR => {
                // A final "*." searches for names without an extension.
                *result.last_mut().unwrap() = DOS_STAR;
            }
            DOT if matches!(units.get(index + 1), Some(&STAR | &QM)) => result.push(DOS_DOT),
            _ => result.push(upcase(unit)),
        }
    }
    result
}

/// Match a Win32 file search expression case-insensitively. Path normalization
/// and short-name aliases belong to the caller, not to this expression matcher.
pub fn wildcard(pattern: &str, name: &str) -> bool {
    if pattern.is_empty() || name.is_empty() {
        return pattern.is_empty() && name.is_empty();
    }
    if matches!(pattern, "*" | "*.*") {
        return true;
    }
    // DOS tokens follow MS-FSA 2.1.4.4. A star before a dot must stop at the
    // final name dot: otherwise "*.?" could skip its extension at EOF. A raw
    // trailing literal dot is not a DOS_DOT wildcard.
    let pattern = expression(pattern);
    let name: Vec<_> = name.encode_utf16().map(upcase).collect();
    let last_dot = name.iter().rposition(|&unit| unit == DOT);
    let mut previous = vec![false; name.len() + 1];
    let mut current = vec![false; name.len() + 1];
    previous[0] = true;
    // Each row represents one expression prefix; each column is a name prefix.
    // Two rows give O(pattern_units * name_units) time and O(name_units) space.
    for unit in pattern {
        for position in 0..=name.len() {
            current[position] = match unit {
                STAR => previous[position] || position > 0 && current[position - 1],
                DOS_STAR => {
                    previous[position]
                        || position > 0 && Some(position - 1) != last_dot && current[position - 1]
                }
                DOS_QM => {
                    // At a dot or EOF, the entire adjacent DOS_QM run can skip
                    // without consuming the dot. Otherwise each takes one unit.
                    previous[position] && (position == name.len() || name[position] == DOT)
                        || position > 0 && name[position - 1] != DOT && previous[position - 1]
                }
                DOS_DOT => {
                    position == name.len() && previous[position]
                        || position > 0 && name[position - 1] == DOT && previous[position - 1]
                }
                _ => position > 0 && name[position - 1] == unit && previous[position - 1],
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[name.len()]
}

#[cfg(test)]
mod tests {
    use super::wildcard;

    #[test]
    fn question_marks_skip_only_at_dots_or_end_of_name() {
        for (pattern, name, expected) in [
            ("patch?.dat", "patch.dat", true),
            ("patch?.dat", "patch1.dat", true),
            ("patch?.dat", "patch12.dat", false),
            ("patch??.dat", "patch.dat", true),
            ("patch??.dat", "patch12.dat", true),
            ("a?", "a", true),
            ("a??", "ab", true),
            ("a??", "abcd", false),
            ("a?.b", "a.b", true),
            ("a??.b", "abc.b", true),
            ("a?c", "abc", true),
            ("a?c", "a.c", false),
            ("a??c", "abc", false),
        ] {
            assert_eq!(wildcard(pattern, name), expected, "{pattern:?}, {name:?}");
        }
    }

    #[test]
    fn optional_extension_and_final_dot_use_dos_tokens() {
        for (pattern, name, expected) in [
            ("*.*", "README", true),
            ("*.", "README", true),
            ("*.", "foo.bar", false),
            ("a*.", "abc", true),
            ("a*.", "a.b.c", false),
            ("foo.*", "foo", true),
            ("foo.*", "foo.bar", true),
            ("foo.*", "foobar", false),
            ("a.*.c", "a.b.c", true),
            ("x*.txt", "x.a.txt", true),
            ("x.*.txt", "x.a.txt", true),
            ("x.*.txt", "x.txt", false),
            ("*.?", "README", true),
            ("*.?", "a.b.c", true),
            ("*.?", "a.txt", false),
            ("<.txt", "x.a.txt", true),
            ("README.", "README", false),
        ] {
            assert_eq!(wildcard(pattern, name), expected, "{pattern:?}, {name:?}");
        }
    }

    #[test]
    fn matching_counts_utf16_units_and_keeps_case_insensitivity() {
        for (pattern, name, expected) in [
            ("??.dat", "漢字.dat", true),
            ("??.dat", "𐐀.dat", true),
            ("?.dat", "漢字.dat", false),
            ("?.dat", "𐐀.dat", false),
            ("?.dat", "漢.dat", true),
            ("save*.dat", "SAVE001.DAT", true),
            ("ä?.dat", "Ä.DAT", true),
            ("*.dat", "save.txt", false),
            ("", "", true),
            ("*", "", false),
            ("", "a", false),
        ] {
            assert_eq!(wildcard(pattern, name), expected, "{pattern:?}, {name:?}");
        }
    }

    #[test]
    fn repeated_stars_with_a_failing_suffix_have_bounded_state() {
        let pattern = format!("{}b", "*a".repeat(128));
        assert!(!wildcard(&pattern, &format!("{}c", "a".repeat(256))));
        assert!(wildcard(&pattern, &format!("{}b", "a".repeat(256))));
    }
}
