//! shlwapi wildcard matching of a whole path against a `;`-separated spec list.
use adv_abi::{AnsiStr, Registry, WideStr, warn_unavailable};
use adv_machine::{Env, Outcome};

use crate::util::set_last_error;

pub(super) fn register(registry: &mut Registry) {
    registry.register(path_match_spec_w_ABI);
    registry.register(path_match_spec_a_ABI);
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
    let (mut at, mut pattern, mut retry) = (0, 0, None);
    // Retain only the most recent wildcard; no recursive suffix expansion or
    // guest-length host stack. The final '?' retains native's optional EOF unit.
    while at < file.len() {
        if spec.get(pattern) == Some(&STAR) {
            pattern += 1;
            retry = Some((pattern, at));
        } else if spec
            .get(pattern)
            .is_some_and(|&unit| unit == QUESTION || same(file[at], unit, ambiguous))
        {
            at += 1;
            pattern += 1;
        } else if let Some((resume, consumed)) = retry {
            at = consumed + 1;
            pattern = resume;
            retry = Some((resume, at));
        } else {
            return false;
        }
    }
    while spec.get(pattern) == Some(&STAR) {
        pattern += 1;
    }
    pattern == spec.len() || spec.get(pattern..) == Some(&[QUESTION][..])
}

#[adv_abi::win32(shlwapi, name = "PathMatchSpecA", level = implement)]
fn path_match_spec_a(env: &mut Env<'_>, file: AnsiStr, spec: AnsiStr) -> Outcome {
    if file.0.is_null() || spec.0.is_null() {
        return Outcome::Return(0);
    }
    let (Ok(file), Ok(spec)) = (file.read(env.cpu, 65536), spec.read(env.cpu, 65536)) else {
        set_last_error(env, 998);
        return Outcome::Return(0);
    };
    let codec = env.ext.get::<crate::Win32>().codec;
    let (file, spec) = (codec.to_utf16(0, &file), codec.to_utf16(0, &spec));
    let mut ambiguous = false;
    let matched = matches(&file, &spec, &mut ambiguous);
    if !matched && ambiguous {
        warn_unavailable(
            env,
            "shlwapi.dll!PathMatchSpecA",
            "non-ASCII case folding differs between Windows releases and locales",
        );
    }
    Outcome::Return(u32::from(matched))
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
    fn repeated_wildcard_failure_and_long_literals_do_not_expand_suffixes_or_stack() {
        let file = wide(&"a".repeat(100));
        let spec = wide(&("*a".repeat(24) + "b"));
        assert!(!matches(&file, &spec, &mut false));
        let file = wide(&"a".repeat(60000));
        assert!(matches(&file, &file, &mut false));
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
