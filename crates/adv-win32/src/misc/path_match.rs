//! shlwapi wildcard matching of a whole path against a `;`-separated spec list.
use adv_abi::{Registry, WideStr, warn_unavailable};
use adv_machine::{Env, Outcome};

use crate::util::set_last_error;

pub(super) fn register(registry: &mut Registry) {
    registry.register(path_match_spec_w_ABI);
}

const STAR: u16 = b'*' as u16;
const QUESTION: u16 = b'?' as u16;
const SEPARATOR: u16 = b';' as u16;
const SPACE: u16 = b' ' as u16;

/// Whether two units are equal under the ASCII case fold. Other case-only
/// differences fold for some characters and not others depending on the
/// Windows release and locale, so they are reported rather than guessed.
fn same(file: u16, spec: u16, ambiguous: &mut bool) -> bool {
    if file == spec {
        return true;
    }
    if file < 0x80 && spec < 0x80 {
        return (file as u8).eq_ignore_ascii_case(&(spec as u8));
    }
    let upper =
        |unit: u16| char::from_u32(u32::from(unit)).map(|c| c.to_uppercase().collect::<String>());
    if file >= 0x80 && spec >= 0x80 && upper(file).is_some_and(|file| Some(file) == upper(spec)) {
        *ambiguous = true;
    }
    false
}

/// One spec: `*` matches any run (separators included) and `?` one unit. A
/// single `?` that ends the spec also matches the end of the name.
fn single(file: &[u16], spec: &[u16], ambiguous: &mut bool) -> bool {
    match spec.split_first() {
        None => file.is_empty(),
        Some((&STAR, rest)) => {
            let rest = &rest[rest.iter().take_while(|&&unit| unit == STAR).count()..];
            (0..=file.len()).any(|skip| single(&file[skip..], rest, ambiguous))
        }
        Some((&QUESTION, rest)) if file.is_empty() => rest.is_empty(),
        Some((&QUESTION, rest)) => single(&file[1..], rest, ambiguous),
        Some((&unit, rest)) => file.split_first().is_some_and(|(&first, tail)| {
            same(first, unit, ambiguous) && single(tail, rest, ambiguous)
        }),
    }
}

/// An empty list matches every name. Each listed spec skips leading spaces
/// (trailing ones are significant), an empty spec matches nothing, and the
/// exact spec `*.*` matches names without an extension too.
fn matches(file: &[u16], specs: &[u16], ambiguous: &mut bool) -> bool {
    if specs.is_empty() {
        return true;
    }
    specs.split(|&unit| unit == SEPARATOR).any(|spec| {
        let start = spec
            .iter()
            .position(|&unit| unit != SPACE)
            .unwrap_or(spec.len());
        let spec = &spec[start..];
        !spec.is_empty() && (spec == [STAR, b'.' as u16, STAR] || single(file, spec, ambiguous))
    })
}

#[adv_abi::win32(shlwapi, name = "PathMatchSpecW", level = implement)]
fn path_match_spec_w(env: &mut Env<'_>, file: WideStr, spec: WideStr) -> Outcome {
    if file.0.is_null() || spec.0.is_null() {
        return Outcome::Return(0);
    }
    let (Ok(file), Ok(spec)) = (file.read(env.cpu, 65536), spec.read(env.cpu, 65536)) else {
        set_last_error(env, 998);
        return Outcome::Return(0);
    };
    let mut ambiguous = false;
    let matched = matches(&file, &spec, &mut ambiguous);
    if !matched && ambiguous {
        warn_unavailable(
            env,
            "shlwapi.dll!PathMatchSpecW",
            "non-ASCII case folding differs between Windows releases and locales",
        );
    }
    Outcome::Return(u32::from(matched))
}

#[cfg(test)]
mod tests {
    use super::matches;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    #[test]
    fn specs_match_the_native_x86_oracle() {
        // (file, spec, native result) from the authored x86 probe.
        let cases = [
            ("file.txt", "*.txt", true),
            ("file.TXT", "*.txt", true),
            ("file", "*", true),
            ("file", "*.*", true),
            (".hidden", "*.*", true),
            ("a.b.c", "*.c", true),
            ("a.b.c", "*.b", false),
            ("file.txt", "f?le.txt", true),
            ("fle.txt", "f?le.txt", false),
            ("file.tx", "file.tx?", true),
            ("file.txt", "*.dat;*.txt", true),
            ("file.txt", "*.dat; *.txt", true),
            ("file.txt", "*.dat ;*.txt", true),
            ("file.txt", ";*.txt", true),
            ("file.txt", "*.txt ", false),
            ("file.txt ", "*.txt", false),
            ("file.txt", "", true),
            ("", "*", true),
            ("", "?", true),
            ("file.txt", ";", false),
            ("file", " ", false),
            ("file", " ;", false),
            (r"C:\dir\file.txt", "*.txt", true),
            (r"C:\dir\file.txt", "file.txt", false),
            (r"C:\dir\file.txt", r"*\file.txt", true),
            (r"C:\dir\file.txt", r"c:\DIR\FILE.TXT", true),
            (r"C:\dir.x\file", "*.*", true),
            (r"C:\dir.x\file", "*.x", false),
            ("file.txt", "*x", false),
            ("file.txt", "*?", true),
            ("ab", "???", true),
            ("abcd", "???", false),
            ("a", "a?", true),
            ("a", "a??", false),
            ("ab", "?", false),
            ("f.txt", "f?.txt", false),
            ("a.b", "a?.b", false),
            ("a.", "a.?", true),
            ("file", "file.*", false),
            ("file", "*.dat;*.*", true),
            ("file", "*.* ", false),
            ("x", "*.**", false),
            ("x", "**", true),
            ("x", "*.?", false),
            ("file", "*.", false),
            ("file.", "*.", true),
            ("a;b", "a;b", false),
            ("a b", "a b", true),
            ("\u{e4}.TXT", "\u{e4}.txt", true),
            ("\u{e4}.txt", "\u{c4}.txt", false),
            ("\u{131}", "I", false),
            ("i", "\u{130}", false),
        ];
        for (file, spec, expected) in cases {
            let mut ambiguous = false;
            assert_eq!(
                matches(&wide(file), &wide(spec), &mut ambiguous),
                expected,
                "{file:?} {spec:?}"
            );
        }
    }

    #[test]
    fn non_ascii_case_variants_are_reported_as_ambiguous() {
        for (file, spec) in [
            ("\u{e4}", "\u{c4}"),
            ("\u{430}", "\u{410}"),
            ("\u{3b1}", "\u{391}"),
        ] {
            let mut ambiguous = false;
            assert!(!matches(&wide(file), &wide(spec), &mut ambiguous));
            assert!(ambiguous, "{file:?} {spec:?}");
        }
        let mut ambiguous = false;
        assert!(!matches(&wide("\u{131}"), &wide("I"), &mut ambiguous));
        assert!(!ambiguous);
    }
}
