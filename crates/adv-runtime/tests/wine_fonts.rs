//! Wine consumes the portable backend's discovered files and missing-family
//! replacements, with real TrueType metadata and pixels from an authored font.
#![cfg(any(target_vendor = "apple", target_os = "android"))]

use adv_runtime::wine::session::{Config, FontOptions, Session, Stop};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

// spec: WINE-SES-5, WINE-SES-8, DEV-FONT-1
#[test]
fn discovered_fonts_and_aliases_survive_prefix_reuse() {
    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    let child = std::env::var_os("ADV_FONT_PROBE_ROOT");
    let root = child.as_ref().map(PathBuf::from).unwrap_or_else(|| {
        std::env::temp_dir().join(format!("adv-wine-fonts-{}", std::process::id()))
    });
    let game = root.join("game");
    let nested = root.join("fonts/nested");
    std::fs::create_dir_all(&game).unwrap();
    std::fs::create_dir_all(&nested).unwrap();
    std::fs::copy(probes.join("fonts.exe"), game.join("fonts.exe")).unwrap();
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../adv-device/tests/fixtures");
    std::fs::copy(
        fixtures.join("font-resource-japanese.ttf"),
        nested.join("japanese.ttf"),
    )
    .unwrap();
    std::fs::copy(fixtures.join("font-resource.ttf"), game.join("private.ttf")).unwrap();
    let fonts = format!(
        r"\??\unix{}",
        root.join("fonts").to_string_lossy().replace('/', "\\")
    );
    if child.is_none() {
        for pass in 0..2 {
            if pass == 0 {
                std::fs::write(game.join("choices.txt"), "explicit").unwrap();
            } else {
                std::fs::remove_file(game.join("choices.txt")).unwrap();
            }
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "discovered_fonts_and_aliases_survive_prefix_reuse",
                    "--nocapture",
                ])
                .env("ADV_FONT_PROBE_ROOT", &root)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::fs::remove_dir_all(root).unwrap();
        return;
    }
    let output = root.join(format!("stdout-{}", std::process::id()));
    let stdout = std::fs::File::create(&output).unwrap();
    let config = Config {
        dist: &dist,
        prefix: &root.join("prefix"),
        drive: None,
        locale: 0x411,
        fonts: &fonts,
        exe: &game.join("fonts.exe"),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: Some(std::os::fd::AsRawFd::as_raw_fd(&stdout)),
    };
    let explicit = game.join("choices.txt").exists();
    let mut fonts = FontOptions::default();
    if explicit {
        fonts.files.push(fixtures.join("font-resource.ttf"));
        fonts
            .overrides
            .families
            .insert("ADV Host Choice".into(), "ADV Resource Fixture".into());
        fonts.overrides.antialiased = Some(false);
    }
    let mut session =
        Session::start_with_fonts(&config, adv_sync::WakeEvent::new().unwrap(), &fonts, |_| {
            Ok(())
        })
        .unwrap();

    let stop = session.run(Instant::now() + Duration::from_secs(30));
    drop(session);
    drop(stdout);
    let text = std::fs::read_to_string(output).unwrap();
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    let expected = if explicit {
        vec!["choices=1", "aliases=1", "pixels=1", "private=1"]
    } else {
        vec!["defaults=1", "aliases=1", "pixels=1", "private=1"]
    };
    assert_eq!(text.lines().collect::<Vec<_>>(), expected);
}
