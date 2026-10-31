//! Wine file and section semantics over the shared ISO reader and install root.
#![cfg(any(target_vendor = "apple", target_os = "android"))]
use adv_runtime::wine::session::{Config, Host, SessionOptions, Stop};
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
                        .starts_with(".hitode-media-"))
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
        exe: &root.join("game/volumes.exe"),
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
