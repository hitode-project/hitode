//! Where tests find real games: the tester's own copies, one directory per PAL build under
//! `$ADV_SAMPLES` (the older `$PAL_SAMPLES` is still accepted):
//!
//! ```text
//! $ADV_SAMPLES/FH2009/  LL1/  LL2/  KS2013/  LL3/  KK2015/  RC2022/  TL2026/
//! ```
//!
//! Each directory is an installed game (the exe, `dll/PAL.dll`, the `.pac` archives). Tests
//! that need a sample skip it when neither variable is set or the directory is missing, so the
//! default test run needs no game files. Nothing is read from or written to a sample.

use std::path::{Path, PathBuf};

/// Every build the samples are organised by, oldest first.
pub const BUILDS: [&str; 8] = [
    "FH2009", "LL1", "LL2", "KS2013", "LL3", "KK2015", "RC2022", "TL2026",
];

/// The game exe when a build's directory holds more than one exe importing PAL.dll.
const EXE: &[(&str, &str)] = &[
    ("FH2009", "FH.exe"),
    ("LL1", "LeyLine1.exe"),
    ("LL2", "LeyLine2.exe"),
    ("KS2013", "kimikishi.exe"),
    ("LL3", "Leyline3.exe"),
    ("KK2015", "koikake.exe"),
    ("RC2022", "red_cherish.exe"),
];

/// `$ADV_SAMPLES` (or `$PAL_SAMPLES`), or `None` (with a note on stderr) when neither is set.
pub fn root() -> Option<PathBuf> {
    let root = std::env::var_os("ADV_SAMPLES")
        .or_else(|| std::env::var_os("PAL_SAMPLES"))
        .map(PathBuf::from);
    if root.is_none() {
        eprintln!("ADV_SAMPLES is not set; tests on local games are not run");
    }
    root
}

/// The directory of `build`'s sample, if present.
pub fn dir(build: &str) -> Option<PathBuf> {
    let dir = root()?.join(build);
    if dir.is_dir() {
        Some(dir)
    } else {
        eprintln!("{build}: no sample at {}", dir.display());
        None
    }
}

/// `rel` (`/`-separated) inside `build`'s sample, matching names case-insensitively as the
/// game does on Windows (`dll/PAL.dll` is `dll/Pal.dll` in later builds).
pub fn file(build: &str, rel: &str) -> Option<PathBuf> {
    find(&dir(build)?, rel)
}

/// `rel` under `base`, each component matched case-insensitively.
pub fn find(base: &Path, rel: &str) -> Option<PathBuf> {
    let mut path = base.to_path_buf();
    for part in rel.split(['/', '\\']).filter(|p| !p.is_empty()) {
        let exact = path.join(part);
        path = if exact.exists() {
            exact
        } else {
            std::fs::read_dir(&path)
                .ok()?
                .flatten()
                .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(part))?
                .path()
        };
    }
    path.exists().then_some(path)
}

/// The game exe of `build`'s sample: the known retail name, else the only root exe that
/// imports PAL.dll.
pub fn exe(build: &str) -> Option<String> {
    let dir = dir(build)?;
    if let Some((_, name)) = EXE.iter().find(|(b, _)| *b == build) {
        return find(&dir, name)
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()));
    }
    let mut found = Vec::new();
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !name.to_ascii_lowercase().ends_with(".exe") {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        let Ok(pe) = adv_pe::Pe::parse(&bytes) else {
            continue;
        };
        let direct = pe.imports().unwrap_or_default().into_iter().map(|d| d.dll);
        let delayed = pe
            .delay_imports()
            .unwrap_or_default()
            .into_iter()
            .map(|d| d.dll);
        if direct
            .chain(delayed)
            .any(|dll| dll.eq_ignore_ascii_case("pal.dll"))
        {
            found.push(name);
        }
    }
    match found.len() {
        1 => found.pop(),
        _ => {
            eprintln!(
                "{build}: {} exes import PAL.dll ({found:?}); expected one",
                found.len()
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn components_match_case_insensitively() {
        let base = std::env::temp_dir().join(format!("adv-samples-{}", std::process::id()));
        std::fs::create_dir_all(base.join("dll")).unwrap();
        std::fs::write(base.join("dll/Pal.dll"), b"").unwrap();
        let found = find(&base, "DLL/PAL.dll").unwrap();
        assert!(found.file_name().unwrap().eq_ignore_ascii_case("pal.dll"));
        assert_eq!(find(&base, "dll/FX.dll"), None);
        std::fs::remove_dir_all(base).unwrap();
    }
}
