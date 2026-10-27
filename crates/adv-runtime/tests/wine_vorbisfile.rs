//! Whole-module admission and callback decoding through Wine's loader.
#![cfg(any(target_vendor = "apple", target_os = "android"))]
use std::path::PathBuf;
use std::time::{Duration, Instant};

use adv_drv::codec::{self, Mode, vorbisfile};
use adv_pe::{Export, ExportTarget, scn, write::DllWriter};
use adv_runtime::wine::session::{Config, Host, SessionOptions, Stop};

fn dll() -> Vec<u8> {
    let mut writer = DllWriter::new("vorbisfile.dll", 0x1800_0000);
    let code = writer
        .section(
            ".text",
            vec![
                0xb8, 77, 0, 0, 0, 0xc3, // Guest exports return 77.
                0xc7, 0x05, 0, 0x20, 0, 0x18, 1, 0, 0, 0, // DllMain sets the flag.
                0xb8, 1, 0, 0, 0, 0xc2, 12, 0, 0xa1, 0, 0x20, 0, 0x18,
                0xc3, // Return the flag.
            ],
            30,
            scn::CNT_CODE | scn::MEM_EXECUTE | scn::MEM_READ,
        )
        .unwrap();
    writer.set_entry(code + 6);
    writer
        .section(".data", vec![0; 4], 4, scn::MEM_READ | scn::MEM_WRITE)
        .unwrap();
    let mut exports: Vec<_> = [
        ("ov_clear", 3),
        ("ov_comment", 4),
        ("ov_info", 5),
        ("ov_open_callbacks", 7),
        ("ov_pcm_seek", 8),
        ("ov_pcm_tell", 10),
        ("ov_pcm_total", 11),
        ("ov_raw_tell", 13),
        ("ov_raw_total", 14),
        ("ov_read", 15),
        ("ov_seekable", 16),
        ("ov_serialnumber", 17),
        ("ov_streams", 18),
        ("ov_test_callbacks", 20),
        ("ov_test_open", 21),
        ("ov_time_seek", 22),
        ("ov_time_tell", 24),
        ("ov_time_total", 25),
    ]
    .into_iter()
    .map(|(name, ordinal)| Export {
        name: Some(name.into()),
        ordinal,
        target: ExportTarget::Rva(code),
    })
    .collect();
    exports.push(Export {
        name: Some("fixture_attached".into()),
        ordinal: 26,
        target: ExportTarget::Rva(code + 24),
    });
    writer.exports(&exports).unwrap();
    writer.relocations(&[code + 8, code + 25]).unwrap();
    writer.finish().unwrap()
}

// spec: CODEC-MODULE-1, DEV-AUDIO-3
#[test]
fn shipped_module_admission_and_callback_decoding() {
    let (Some(dist), Some(probes)) = (
        std::env::var_os("ADV_WINE_DIST").map(PathBuf::from),
        std::env::var_os("ADV_WINE_PROBES").map(PathBuf::from),
    ) else {
        eprintln!("skipped: ADV_WINE_DIST or ADV_WINE_PROBES is not set");
        return;
    };
    let root = std::env::temp_dir().join(format!("adv-wine-vorbisfile-{}", std::process::id()));
    let game = root.join("game");
    std::fs::create_dir_all(&game).unwrap();
    let original = dll();
    // Wine's namespace resolves the shipped DLL regardless of host filename case.
    let library = game.join("VorbisFile.DLL");
    std::fs::write(&library, &original).unwrap();
    let probe = std::fs::read(probes.join("vorbisfile.exe")).unwrap();
    let callback_probe = std::fs::read(probes.join("vorbis_codec.exe")).unwrap();
    let data = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data");
    let mut host = Host::default();
    for (case, mode, jit) in [
        ("admitted", Mode::Native, true),
        ("dynamic", Mode::Native, true),
        ("guest", Mode::Guest, true),
        ("verify", Mode::Verify, true),
        ("trace", Mode::Native, true),
        ("callbacks", Mode::Native, true),
        ("interpreter", Mode::Native, false),
    ] {
        let mut bytes = if case == "callbacks" {
            callback_probe.clone()
        } else {
            probe.clone()
        };
        if case == "dynamic" {
            bytes.extend_from_slice(b"\0ov_read_float\0");
        }
        let exe = game.join("probe.exe");
        std::fs::write(&exe, bytes).unwrap();
        std::fs::copy(data.join("tone-mono.ogg"), game.join("fixture_codec.ogg")).unwrap();
        let prefix = root.join("prefix");
        let config = Config {
            dist: &dist,
            prefix: &prefix,
            drive: None,
            locale: 0x409,
            fonts: "",
            exe: &exe,
            width: 64,
            height: 32,
            jit,
            debug: "",
            stdout: None,
        };
        let mut session = host
            .start(
                &config,
                adv_sync::WakeEvent::new().unwrap(),
                &SessionOptions::default(),
                |machine| {
                    codec::install(&mut machine.ext, mode);
                    Ok(())
                },
            )
            .unwrap();
        let until = Instant::now() + Duration::from_secs(60);
        let stop = loop {
            let stop = session.run(Instant::now() + Duration::from_millis(5));
            if !matches!(stop, Stop::Paused) || Instant::now() >= until {
                break stop;
            }
        };
        let guest = case == "dynamic" || mode == Mode::Guest;
        let selection = session.machine.ext.get::<vorbisfile::Selection>();
        assert_eq!(
            selection.mode == Mode::Guest,
            guest,
            "{case}: {selection:?}"
        );
        if case == "dynamic" {
            assert!(
                selection
                    .reason
                    .as_deref()
                    .unwrap()
                    .contains("ov_read_float")
            );
        }
        assert!(
            matches!(stop, Stop::Exited(code) if code == if guest { 77 } else { 0 }),
            "{case}: {stop:?}"
        );
        let stats = vorbisfile::stats(&session.machine);
        assert_eq!(stats.is_none(), guest, "{case}");
        if case == "callbacks" {
            let stats = stats.unwrap();
            assert_eq!(
                (
                    stats.opens,
                    stats.failed_opens,
                    stats.clears,
                    stats.active_streams
                ),
                (8, 1, 7, 0)
            );
            assert_eq!(stats.close_callbacks, 6);
            assert!(
                stats.decoded_packets > 0
                    && stats.decoded_frames >= 11025
                    && stats.callback_bytes > 0
            );
            assert_eq!(
                stats.guest_callbacks,
                stats.read_callbacks
                    + stats.seek_callbacks
                    + stats.tell_callbacks
                    + stats.close_callbacks
            );
            let actual = std::fs::read(game.join("output.pcm")).unwrap();
            let reference = std::fs::read(data.join("tone-mono.pcm")).unwrap();
            assert_eq!(actual.len(), 11025 * 2);
            let (energy, error) = reference
                .as_chunks::<2>()
                .0
                .iter()
                .zip(actual.as_chunks::<2>().0)
                .fold((0.0, 0.0), |(energy, error), (r, a)| {
                    let r = f64::from(i16::from_le_bytes(*r));
                    let a = f64::from(i16::from_le_bytes(*a));
                    (energy + r * r, error + (r - a) * (r - a))
                });
            assert!(10.0 * (energy / error.max(1e-30)).log10() > 60.0);
            let seeked = std::fs::read(game.join("seek.pcm")).unwrap();
            assert_eq!(seeked, actual[2205 * 2..2205 * 2 + seeked.len()]);
        }
        assert_eq!(std::fs::read(&library).unwrap(), original, "{case}");
        eprintln!("{case}: {stop:?}");
        drop(session);
    }
    drop(host);
    std::fs::remove_dir_all(root).unwrap();
}
