//! Wine file and section semantics over the shared ISO reader and install root,
//! and installers run from a disc.
#![cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_runtime::wine::session::{Config, Host, Image, Program, SessionOptions, Stop};
use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;
use std::time::{Duration, Instant};

#[allow(dead_code)]
mod wine {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../adv-nt/tests/common/wine.rs"
    ));
}

const IMAGE: &[u8] = include_bytes!("../../adv-os/tests/fixtures/optical.iso");

/// Append authored PE files to the fixture's Joliet root without changing its
/// original files. Both endian copies of the volume and record sizes agree.
fn with_modules(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    fn both(bytes: &mut [u8], value: u32) {
        bytes[..4].copy_from_slice(&value.to_le_bytes());
        bytes[4..8].copy_from_slice(&value.to_be_bytes());
    }
    let mut image = IMAGE.to_vec();
    let mut at = 21 * 2048;
    while image[at] != 0 {
        at += image[at] as usize;
    }
    let template = image[21 * 2048 + 68..21 * 2048 + 102].to_vec();
    for (name, contents) in files {
        let lba = image.len() / 2048;
        image.extend_from_slice(contents);
        image.resize(image.len().next_multiple_of(2048), 0);
        let name: Vec<_> = name.encode_utf16().flat_map(u16::to_be_bytes).collect();
        let length = 34 + name.len();
        image[at..at + 34].copy_from_slice(&template);
        image[at] = length as u8;
        both(&mut image[at + 2..at + 10], lba as u32);
        both(&mut image[at + 10..at + 18], contents.len() as u32);
        image[at + 32] = name.len() as u8;
        image[at + 33..at + 33 + name.len()].copy_from_slice(&name);
        image[at + length - 1] = 0;
        at += length;
    }
    let sectors = (image.len() / 2048) as u32;
    for sector in [16, 17] {
        both(&mut image[sector * 2048 + 80..sector * 2048 + 88], sectors);
    }
    image
}

// spec: DEV-DISC-1, DEV-DISC-3, DEV-DISC-4, DEV-DISC-6, DEV-DISC-7, WINE-SES-1, WINE-SES-4, WINE-SES-9, WINE-SES-10
#[test]
fn mounted_volumes_and_install_paths_survive_processes_and_prefix_reuse() {
    let Some(wine::Wine { dist, probes }) = wine::wine() else {
        return;
    };
    if std::env::var_os("ADV_NT_PROBE_CHILD").is_none() {
        let root = std::env::temp_dir().join(format!("adv-wine-volumes-{}", std::process::id()));
        let game = root.join("game");
        std::fs::create_dir_all(&game).unwrap();
        std::fs::copy(probes.join("volumes.exe"), game.join("volumes.exe")).unwrap();
        std::fs::write(root.join("fixture.iso"), IMAGE).unwrap();
        let extended = with_modules(&[
            (
                "Probe.dll",
                std::fs::read(probes.join("volumes_module.dll")).unwrap(),
            ),
            (
                "FromDisc.exe",
                std::fs::read(probes.join("volumes.exe")).unwrap(),
            ),
        ]);
        std::fs::write(root.join("modules.iso"), extended).unwrap();
        let original = root.join("prefix/drive_c/Program Files/original.txt");
        std::fs::create_dir_all(original.parent().unwrap()).unwrap();
        std::fs::write(&original, "prefix content").unwrap();
        std::os::unix::fs::symlink(root.join("prefix"), root.join("prefix-alias")).unwrap();
        for reset in [false, true] {
            if reset {
                std::fs::write(game.join("reset.signal"), []).unwrap();
            }
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "mounted_volumes_and_install_paths_survive_processes_and_prefix_reuse",
                    "--nocapture",
                ])
                .env("ADV_NT_PROBE_CHILD", "1")
                .current_dir(&root)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                std::fs::read_to_string(&original).unwrap(),
                "prefix content"
            );
            assert!(
                !root
                    .join("prefix/drive_c/Program Files/Volume Probe")
                    .exists()
            );
            assert_eq!(
                std::fs::read(root.join("install/Volume Probe/renamed.bin")).unwrap(),
                b"installed"
            );
            assert_eq!(
                std::fs::read(root.join("install/Volume Probe/copied.bin")).unwrap(),
                b"abcde"
            );
            assert!(
                !std::fs::read_dir(root.join("prefix"))
                    .unwrap()
                    .any(|entry| entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".hitode-files-"))
            );
        }
        std::fs::remove_dir_all(root).unwrap();
        return;
    }
    let root = std::env::current_dir().unwrap();
    let reset = root.join("game/reset.signal").exists();
    let stdout = std::fs::File::create(root.join("stdout")).unwrap();
    let config = Config {
        dist: &dist,
        prefix: &root.join("prefix-alias"),
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: Some(&root.join("game/volumes.exe")),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: Some(stdout.as_raw_fd()),
    };
    let mut options = SessionOptions::default();
    if !reset {
        options.cdroms = vec![
            ('D', root.join("fixture.iso")),
            ('G', root.join("modules.iso")),
        ];
        options.install_dir = Some(root.join("install"));
    }
    let mut host = Host::default();
    let mut session = host
        .start(
            &config,
            adv_sync::WakeEvent::new().unwrap(),
            &options,
            |_| Ok(()),
        )
        .unwrap();
    let stop = session.run(Instant::now() + Duration::from_secs(30));
    let text = std::fs::read_to_string(root.join("stdout")).unwrap();
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert!(!text.contains("FAIL"), "{text}");
    if !reset {
        let devices = root.join("prefix/dosdevices");
        let placeholder = std::fs::read_link(devices.join("d:"))
            .unwrap()
            .join("ReadMe.txt");
        assert_eq!(
            std::fs::metadata(placeholder).unwrap().blocks(),
            0,
            "identity file must contain no extracted data"
        );
    }
    drop(session);
    drop(stdout);
    if reset {
        assert!(text.contains("reset=ok"), "{text}");
    } else {
        for check in [
            "joliet",
            "mapped_data",
            "private_map",
            "unchanged",
            "multi_extent",
            "inherited_read",
            "inherited_map",
            "inherited_install",
        ] {
            assert!(text.contains(&format!("{check}=ok")), "{check}: {text}");
        }
        let mut columns = [0u8; 4];
        for (i, byte) in IMAGE[17 * 2048..18 * 2048].iter().enumerate() {
            columns[i % 4] = columns[i % 4].wrapping_add(*byte);
        }
        assert!(
            text.contains(&format!(
                "serial={} capacity={}",
                u32::from_be_bytes(columns),
                IMAGE.len()
            )),
            "{text}"
        );
        std::fs::write(root.join("game/reset.signal"), []).unwrap();
        let output = std::fs::File::create(root.join("stdout-reset")).unwrap();
        let config = Config {
            stdout: Some(output.as_raw_fd()),
            ..config
        };
        let mut session = host
            .start(
                &config,
                adv_sync::WakeEvent::new().unwrap(),
                &SessionOptions::default(),
                |_| Ok(()),
            )
            .unwrap();
        let stop = session.run(Instant::now() + Duration::from_secs(30));
        drop(session);
        drop(output);
        let text = std::fs::read_to_string(root.join("stdout-reset")).unwrap();
        assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
        assert!(text.contains("reset_namespace=ok"), "{text}");
    }
}

/// One wineserver runs per process: true in the test's own child process,
/// after the parent has started it and checked that it passed.
fn in_own_process(test: &str) -> bool {
    if std::env::var_os("ADV_NT_PROBE_CHILD").is_some() {
        return true;
    }
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("ADV_NT_PROBE_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    false
}

/// An uncompressed cabinet holding `files`, as setup programs unpack.
fn cabinet(files: &[(&str, &[u8])]) -> Vec<u8> {
    let names: usize = files.iter().map(|(name, _)| 16 + name.len() + 1).sum();
    let data: Vec<u8> = files
        .iter()
        .flat_map(|(_, bytes)| bytes.iter().copied())
        .collect();
    let first_file = 36 + 8;
    let first_data = first_file + names;
    let total = first_data + 8 + data.len();
    let mut cab = Vec::with_capacity(total);
    cab.extend_from_slice(b"MSCF");
    for value in [0, total as u32, 0, first_file as u32, 0] {
        cab.extend_from_slice(&value.to_le_bytes());
    }
    cab.extend_from_slice(&[3, 1]);
    for value in [1u16, files.len() as u16, 0, 0x1234, 0] {
        cab.extend_from_slice(&value.to_le_bytes());
    }
    cab.extend_from_slice(&(first_data as u32).to_le_bytes());
    cab.extend_from_slice(&1u16.to_le_bytes());
    cab.extend_from_slice(&0u16.to_le_bytes());
    let mut offset = 0u32;
    for (name, bytes) in files {
        cab.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        cab.extend_from_slice(&offset.to_le_bytes());
        // Folder 0, 2026-01-01 12:00, archive.
        for value in [0u16, (46 << 9) | (1 << 5) | 1, 12 << 11, 0x20] {
            cab.extend_from_slice(&value.to_le_bytes());
        }
        cab.extend_from_slice(name.as_bytes());
        cab.push(0);
        offset += bytes.len() as u32;
    }
    cab.extend_from_slice(&0u32.to_le_bytes());
    cab.extend_from_slice(&(data.len() as u16).to_le_bytes());
    cab.extend_from_slice(&(data.len() as u16).to_le_bytes());
    cab.extend_from_slice(&data);
    cab
}

// spec: WINE-SES-10, WINE-SES-12, WINE-SES-14
#[test]
fn a_disc_setup_installs_a_game_that_then_runs_from_drive_c() {
    let Some(wine::Wine { dist, probes }) = wine::wine() else {
        return;
    };
    if !in_own_process("a_disc_setup_installs_a_game_that_then_runs_from_drive_c") {
        return;
    }
    let root = std::env::temp_dir().join(format!("adv-wine-install-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let installer = std::fs::read(probes.join("installer.exe")).unwrap();
    let disc = with_modules(&[
        ("SETUP.EXE", installer.clone()),
        ("GAME.EXE", installer),
        (
            "DATA.CAB",
            cabinet(&[("readme.txt", b"from a cabinet"), ("data.bin", &[7; 3000])]),
        ),
    ]);
    std::fs::write(root.join("disc.iso"), disc).unwrap();
    let prefix = root.join("prefix");
    let run =
        |host: &mut Host, exe: Option<&std::path::Path>, options: &SessionOptions, log: &str| {
            let stdout = std::fs::File::create(root.join(log)).unwrap();
            let config = Config {
                dist: &dist,
                prefix: &prefix,
                drive: None,
                locale: 0x409,
                fonts: "",
                exe,
                width: 640,
                height: 480,
                jit: true,
                debug: "",
                stdout: Some(stdout.as_raw_fd()),
            };
            let mut session = host
                .start(
                    &config,
                    adv_sync::WakeEvent::new().unwrap(),
                    options,
                    |_| Ok(()),
                )
                .unwrap();
            let stop = session.run(Instant::now() + Duration::from_secs(60));
            drop(session);
            let text = std::fs::read_to_string(root.join(log)).unwrap();
            assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
            assert!(!text.contains("FAIL"), "{text}");
            text
        };

    // No game folder: the setup's own directory on the disc is where it starts.
    let output = std::fs::File::create(root.join("setup")).unwrap();
    let mut host = Host::default();
    let setup = SessionOptions {
        cdroms: vec![('D', root.join("disc.iso"))],
        program: Some(Program {
            output: Some(output.as_raw_fd()),
            ..Program::new(Image::Guest(r"D:\SETUP.EXE".into())) // privacy: allow authored guest optical path
        }),
        ..SessionOptions::default()
    };
    run(&mut host, None, &setup, "setup-stdout");
    let text = std::fs::read_to_string(root.join("setup")).unwrap();
    assert!(
        text.contains("stage_exit=0") && text.contains("msiexec=ok"),
        "{text}"
    );
    let installed = prefix.join("drive_c/Program Files/Probe Vendor/Probe Game");
    assert_eq!(
        std::fs::read(installed.join("readme.txt")).unwrap(),
        b"from a cabinet"
    );
    assert_eq!(
        std::fs::read(installed.join("data.bin")).unwrap(),
        [7; 3000]
    );
    let system = std::fs::read_to_string(prefix.join("system.reg")).unwrap();
    assert!(
        system.contains(r"[Software\\Probe Vendor\\Probe Game]"),
        "{system}"
    );
    assert!(
        system.contains(r"[Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\ProbeGame]")
    );
    // Its uninstall entry tells where it went.
    let config = Config {
        dist: &dist,
        prefix: &prefix,
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: None,
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: None,
    };
    let found = host.installed(&config, &SessionOptions::default()).unwrap();
    assert!(
        found.iter().any(|program| program.name == "Probe Game"
            && program.folder == installed.canonicalize().unwrap()),
        "{found:?}"
    );

    // The installed game keeps its drive C: path, so what the setup wrote
    // about it holds.
    let text = run(
        &mut host,
        Some(&installed.join("game.exe")),
        &SessionOptions::default(),
        "game-stdout",
    );
    assert!(text.contains("played=ok"), "{text}");
    std::fs::remove_dir_all(root).unwrap();
}

// spec: WINE-SES-12, WINE-UI-7, WINE-UI-10
#[test]
fn a_setup_wizard_shows_natively_and_a_tree_falls_back_to_its_painting() {
    use adv_runtime::{ControlKind, DialogEvent};
    let Some(wine::Wine { dist, probes }) = wine::wine() else {
        return;
    };
    if !in_own_process("a_setup_wizard_shows_natively_and_a_tree_falls_back_to_its_painting") {
        return;
    }
    let root = std::env::temp_dir().join(format!("adv-wine-wizard-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let stdout = std::fs::File::create(root.join("stdout")).unwrap();
    let config = Config {
        dist: &dist,
        prefix: &root.join("prefix"),
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: Some(&probes.join("wizard.exe")),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: Some(stdout.as_raw_fd()),
    };
    let mut host = Host::default();
    let mut session = host
        .start(
            &config,
            adv_sync::WakeEvent::new().unwrap(),
            &SessionOptions::default(),
            |_| Ok(()),
        )
        .unwrap();
    let until = Instant::now() + Duration::from_secs(60);
    let mut pressed = Vec::new();
    let mut unavailable = std::collections::BTreeSet::new();
    let (mut license, mut banner, mut progress, mut browser) = (false, false, false, false);
    let stop = loop {
        assert!(Instant::now() < until, "the wizard was not answered");
        // Each page is answered once it says it is ready. Snapshots arrive
        // after the guest answers them, so the last page waits until one
        // shows its Finish button; the sheet's other buttons serve every page.
        let ready = std::fs::read_to_string(root.join("stdout"))
            .unwrap()
            .matches("page=")
            .count();
        session.refresh_ui();
        let dialogs = session.dialogs().dialogs;
        let painted = session
            .input_view()
            .windows
            .iter()
            .any(|window| window.parent == 0 && window.visible && window.text == "Browse");
        if painted {
            assert!(
                dialogs.is_empty(),
                "a tree dialog stays the guest's: {dialogs:?}"
            );
            browser = true;
        }
        if let Some(dialog) = dialogs.first() {
            for control in dialog.controls.iter().filter(|control| control.visible) {
                match control.kind {
                    ControlKind::Unavailable => {
                        unavailable.insert(control.class.clone());
                    }
                    ControlKind::Text => {
                        license |= control.text.len() > 1000 && control.text.ends_with("The end.");
                    }
                    ControlKind::Image => {
                        let image = control.image.as_ref().unwrap();
                        assert_eq!((image.width, image.height), (4, 2));
                        assert_eq!(image.bgra[..8], [255, 0, 0, 255, 0, 255, 0, 255]);
                        assert_eq!(image.bgra[16..20], [0, 0, 0, 255]);
                        banner = true;
                    }
                    ControlKind::Progress => {
                        progress |= control.value == 100 && control.range == Some((0, 100));
                    }
                    _ => {}
                }
            }
            let default = dialog.controls.iter().find(|control| {
                control.kind == ControlKind::Push
                    && control.style & 0xf == 1
                    && control.visible
                    && control.enabled
            });
            let expected = if pressed.len() == 2 {
                "Finish"
            } else {
                "&Next >"
            };
            if let Some(button) = default
                && ready > pressed.len()
                && button.text == expected
            {
                pressed.push(button.text.clone());
                session.dialog_event(DialogEvent::Click(button.target));
            }
        }
        match session.run(Instant::now() + Duration::from_millis(20)) {
            Stop::Paused => {}
            stop => break stop,
        }
    };
    drop(session);
    let text = std::fs::read_to_string(root.join("stdout")).unwrap();
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert!(
        text.contains("page=2") && text.contains("finished=1"),
        "{text}"
    );
    assert!(unavailable.is_empty(), "{unavailable:?}");
    assert!(license && banner && progress && browser, "{text}");
    assert_eq!(pressed, ["&Next >", "&Next >", "Finish"]);
    std::fs::remove_dir_all(root).unwrap();
}

/// `image` with volume label `label` in its primary and Joliet descriptors.
fn labelled(mut image: Vec<u8>, label: &str) -> Vec<u8> {
    let primary = &mut image[16 * 2048 + 40..16 * 2048 + 72];
    primary.fill(b' ');
    primary[..label.len()].copy_from_slice(label.as_bytes());
    let joliet: Vec<u8> = format!("{label:<16}")
        .encode_utf16()
        .flat_map(u16::to_be_bytes)
        .collect();
    image[17 * 2048 + 40..17 * 2048 + 72].copy_from_slice(&joliet);
    image
}

// spec: WINE-SES-13
#[test]
fn a_disc_changes_while_a_setup_waits_and_kept_files_read_the_first() {
    let Some(wine::Wine { dist, probes }) = wine::wine() else {
        return;
    };
    if !in_own_process("a_disc_changes_while_a_setup_waits_and_kept_files_read_the_first") {
        return;
    }
    let root = std::env::temp_dir().join(format!("adv-wine-swap-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for (name, label, text) in [
        ("one.iso", "DISC1", "first"),
        ("two.iso", "DISC2", "second"),
    ] {
        let image = with_modules(&[("DISC.TXT", text.as_bytes().to_vec())]);
        std::fs::write(root.join(name), labelled(image, label)).unwrap();
    }
    let log = root.join("stdout");
    let stdout = std::fs::File::create(&log).unwrap();
    let config = Config {
        dist: &dist,
        prefix: &root.join("prefix"),
        drive: None,
        locale: 0x409,
        fonts: "",
        exe: Some(&probes.join("discswap.exe")),
        width: 640,
        height: 480,
        jit: true,
        debug: "",
        stdout: Some(stdout.as_raw_fd()),
    };
    let options = SessionOptions {
        cdroms: vec![('D', root.join("one.iso"))],
        ..SessionOptions::default()
    };
    let mut host = Host::default();
    let mut session = host
        .start(
            &config,
            adv_sync::WakeEvent::new().unwrap(),
            &options,
            |_| Ok(()),
        )
        .unwrap();
    let until = Instant::now() + Duration::from_secs(60);
    let mut changed = false;
    let stop = loop {
        assert!(Instant::now() < until, "the setup did not finish");
        if !changed && std::fs::read_to_string(&log).unwrap().contains("insert=2") {
            session.change_disc('d', &root.join("two.iso")).unwrap();
            changed = true;
        }
        match session.run(Instant::now() + Duration::from_millis(20)) {
            Stop::Paused => {}
            stop => break stop,
        }
    };
    drop(session);
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(matches!(stop, Stop::Exited(0)), "{stop:?}: {text}");
    assert!(
        changed
            && text.contains("label=DISC2")
            && text.contains("second=ok")
            && text.contains("kept=ok"),
        "{text}"
    );
    std::fs::remove_dir_all(root).unwrap();
}
