//! Wine's own conformance tests on the Wine userland: each case is one test
//! of a Wine test module, linked alone so that it runs without arguments, and
//! it must finish with no failures.
//!
//! It needs `ADV_WINE_DIST` and `ADV_WINETEST_DIR`, the directory that holds
//! `<module>_<test>.exe`; without them it is skipped. The server runs once
//! per process, so each case runs in a child process of this test binary.
#![cfg(target_vendor = "apple")]

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use adv_runtime::wine::session::{Config, Session, Stop};

/// Cases that pass on standard Wine, as `<module>_<test>`.
const CASES: &[&str] = &[
    "gdi32_brush",
    "gdi32_clipping",
    "gdi32_gdiobj",
    "gdi32_mapping",
    "gdi32_palette",
    "gdi32_path",
    "gdi32_pen",
    "kernel32_atom",
    "kernel32_codepage",
    "kernel32_format_msg",
    "msvcrt_heap",
    "msvcrt_locale",
    "msvcrt_string",
    "msvcrt_time",
    "ntdll_atom",
    "ntdll_env",
    "ntdll_error",
    "ntdll_large_int",
    "ntdll_rtlstr",
    "ntdll_string",
    "user32_dce",
    "user32_text",
];
const CASE: &str = "ADV_WINETEST_CASE";
const WORKERS: usize = 8;

// spec: WINE-TEST-1
#[test]
fn wine_conformance_cases_pass() {
    if std::env::var_os(CASE).is_some() {
        return;
    }
    if std::env::var_os("ADV_WINE_DIST").is_none() || std::env::var_os("ADV_WINETEST_DIR").is_none()
    {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINETEST_DIR is not set");
        return;
    }
    let exe = std::env::current_exe().unwrap();
    let queue = std::sync::Mutex::new(CASES.iter());
    let failed = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            scope.spawn(|| {
                while let Some(&case) = queue.lock().unwrap().next() {
                    let output = Command::new(&exe)
                        .args(["run_case", "--exact", "--nocapture"])
                        .env(CASE, case)
                        .output()
                        .unwrap();
                    let text = String::from_utf8_lossy(&output.stdout);
                    let summary = text
                        .lines()
                        .rev()
                        .find(|line| line.contains("tests executed"));
                    let passed = output.status.success()
                        && summary.is_some_and(|line| line.contains(" 0 failures"));
                    if !passed {
                        let summary = summary.unwrap_or("no summary").to_owned();
                        failed.lock().unwrap().push(format!("{case}: {summary}"));
                    }
                }
            });
        }
    });
    let failed = failed.into_inner().unwrap();
    assert!(failed.is_empty(), "{failed:#?}");
}

/// One case, in a child process.
#[test]
fn run_case() {
    let Some(case) = std::env::var_os(CASE) else {
        return;
    };
    let case = case.into_string().unwrap();
    let dist = PathBuf::from(std::env::var_os("ADV_WINE_DIST").unwrap());
    let tests = PathBuf::from(std::env::var_os("ADV_WINETEST_DIR").unwrap());
    let root = std::env::temp_dir().join(format!("adv-winetest-{case}-{}", std::process::id()));
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    let name = format!("{case}.exe");
    std::fs::copy(tests.join(&name), game.join(&name)).unwrap();
    let session_root = root.join("session");
    let config = Config {
        dist: &dist,
        root: &session_root,
        exe: &game.join(&name),
        width: 1024,
        height: 768,
        jit: true,
        debug: "",
        stdout: Some(1),
    };
    let wake = adv_sync::WakeEvent::new().unwrap();
    let mut session = Session::start(&config, wake, |_| Ok(())).unwrap();
    let stop = session.run(Instant::now() + Duration::from_secs(300));
    drop(session);
    std::fs::remove_dir_all(&root).unwrap();
    assert!(matches!(stop, Stop::Exited(0)), "{case}: {stop:?}");
}
