//! The text probe: gdi32 and win32u.so draw a TrueType font through
//! FreeType into a DIB section.
//!
//! Besides the Wine build, the test needs an i386 `libfreetype.so.6` in
//! its `lib/wine/i386-unix` and the fonts in `share/wine/fonts`.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

#[path = "../../adv-nt/tests/common/mod.rs"]
mod common;

use adv_machine::RunResult;

// spec: WINE-SYS-5, WINE-UNIX-4
#[test]
fn gdi_draws_truetype_text() {
    let Some((result, output)) =
        common::run_probe_on("text.exe", false, adv_unix::Platform, |machine| {
            adv_unix::install(machine)
        })
    else {
        return;
    };
    assert!(
        matches!(result, RunResult::Exited(0)),
        "{result:?}\n{output}"
    );
    // The numbers Wine prints for the probe with the same fonts.
    assert_eq!(
        output.lines().collect::<Vec<_>>(),
        ["face=Tahoma width=15 height=19 drawn=1 inked=43"]
    );
}
