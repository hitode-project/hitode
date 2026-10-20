//! Recorded native results of authored fixtures replay on every host and backend.
//!
//! `tools/fixtures/record_native.py` builds each fixture deterministically on
//! Windows, runs it natively and stores the executable with the scalar output
//! it wrote in `tools/fixtures/native/<name>/`. Here each executable runs under
//! every CPU backend with a virtual clock, and its output must match the
//! native recording word by word. `tools/check.py` rejects a recording whose
//! fixture source changed since it was made.
use adv_machine::{Clock, RunResult};
use adv_runtime::boot::Runtime;
use std::path::{Path, PathBuf};
#[path = "support/cpu.rs"]
mod cpu;

/// Recordings that do not match yet on non-Windows hosts, with what differs.
/// A listed recording still runs, and fails the test once it matches so the
/// entry is removed when its cause is fixed.
const KNOWN: &[(&str, &str)] = &[
    (
        "drawtext-lines",
        "glyph pixel hashes differ where host fonts stand in",
    ),
    (
        "nls-compare",
        "linguistic comparison results differ from Windows",
    ),
    (
        "window-property",
        "three property scalars differ from Windows",
    ),
];

/// (executable, native output) of one recording folder.
fn recording(folder: &Path) -> (PathBuf, PathBuf) {
    let mut files: Vec<_> = std::fs::read_dir(folder)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    files.sort();
    let pick = |extension: &str| {
        let found: Vec<_> = files
            .iter()
            .filter(|path| path.extension().is_some_and(|e| e == extension))
            .collect();
        assert_eq!(
            found.len(),
            1,
            "{}: one .{extension} file",
            folder.display()
        );
        found[0].clone()
    };
    (pick("exe"), pick("bin"))
}

#[test]
// spec: WIN32-API-11, WIN32-API-12, WIN32-GDI-14, WIN32-INPUT-1
// spec: WIN32-OBJECT-1, WIN32-OBJECT-2, WIN32-OBJECT-3
fn recorded_fixtures_match_native_windows_on_every_backend() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut folders: Vec<_> = std::fs::read_dir(root.join("tools/fixtures/native"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.is_dir())
        .collect();
    folders.sort();
    assert!(!folders.is_empty());
    let mut failures = Vec::new();
    for folder in folders {
        let name = folder.file_name().unwrap().to_string_lossy().into_owned();
        let known = !cfg!(windows) && KNOWN.iter().any(|(known, _)| *known == name);
        let before = failures.len();
        let (exe, native) = recording(&folder);
        let expected = std::fs::read(&native).unwrap();
        let output = native.file_name().unwrap();
        for backend in ["interpreter", "jit", "unicorn"] {
            let data = root
                .join("target/investigations/native-reference")
                .join(&name)
                .join(backend);
            let _ = std::fs::remove_dir_all(&data);
            let mut runtime =
                Runtime::with_cpu(&exe, &data, cpu::Cpu::new(backend).unwrap(), 32, 32).unwrap();
            runtime.machine.sys.clock = Clock::virtual_time();
            let result = runtime.machine.run_virtual(10_000_000);
            if !matches!(result, RunResult::Exited(0)) {
                failures.push(format!("{name} {backend}: {result:?}"));
                continue;
            }
            let actual = std::fs::read(data.join("overlay").join(output)).unwrap_or_default();
            if actual.len() != expected.len() {
                failures.push(format!(
                    "{name} {backend}: {} bytes, native {}",
                    actual.len(),
                    expected.len()
                ));
                continue;
            }
            let differing: Vec<_> = actual
                .as_chunks::<4>()
                .0
                .iter()
                .zip(expected.as_chunks::<4>().0.iter())
                .enumerate()
                .filter(|(_, (a, e))| a != e)
                .map(|(index, _)| index)
                .collect();
            if !differing.is_empty() {
                failures.push(format!(
                    "{name} {backend}: scalars {differing:?} differ from native"
                ));
            }
        }
        if known {
            if failures.len() == before {
                failures.push(format!("{name} now matches native; remove it from KNOWN"));
            } else {
                let notes: Vec<_> = failures.drain(before..).collect();
                eprintln!("known difference: {}", notes.join("; "));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
