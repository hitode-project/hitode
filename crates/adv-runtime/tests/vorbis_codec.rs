//! Authored callback/seek/lifetime fixture on native Windows, Unicorn,
//! and the repository's interpreter/JIT CPUs.
//! Inputs and reference PCM are short sine tones generated with FFmpeg/libvorbis.
#![cfg(windows)]
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use adv_machine::{Clock, RunResult, SliceResult};
use adv_runtime::boot::Runtime;
use adv_runtime::vorbis::Policy;
#[path = "support/cpu.rs"]
mod cpu;

const NANA: &str = "七色★星露";

/// `directory` under `$ADV_SAMPLES`, when the tester has that game installed.
fn sample(directory: &str) -> Option<PathBuf> {
    adv_samples::root()
        .map(|root| root.join(directory))
        .filter(|path| path.is_dir())
}

fn samples(bytes: &[u8]) -> Vec<f64> {
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| f64::from(i16::from_le_bytes(*p)))
        .collect()
}
fn snr(reference: &[u8], actual: &[u8]) -> f64 {
    let mut energy = 0.0;
    let mut error = 0.0;
    for (r, a) in samples(reference).into_iter().zip(samples(actual)) {
        energy += r * r;
        error += (r - a) * (r - a)
    }
    10.0 * (energy / error.max(1e-30)).log10()
}

#[test]
#[ignore = "read-only preflight against a locally installed authorized game"]
fn real_codec_capability_preflight_reports_actual_provider() {
    let Some(game) = sample(NANA) else { return };
    let files = adv_os::loader::HostDir::new(game, "C:\\Game");
    let selected =
        adv_runtime::vorbis::select_policy_with_reason(&files, "nana_chs.exe", Policy::Native)
            .unwrap();
    eprintln!("real Nana codec preflight: {selected:?}");
    assert_eq!(selected.policy, Policy::Native, "{:?}", selected.reason);
}

#[test]
#[ignore = "read-only preflight against locally installed authorized cross-engine games"]
fn cross_engine_preflight_is_observable_without_rejecting_guest_startup() {
    let Some(games) = adv_samples::root() else {
        return;
    };
    for (directory, exe) in [
        ("AQUA", "AQUA.exe"),
        ("KIMIKISHI", "kimikishi.exe"),
        (
            "ディメンション凸ラバース！！",
            "DimensionToTsuLovers_cn.exe",
        ),
    ] {
        let files = adv_os::Vfs::new(&adv_os::OsConfig::new(
            games.join(directory),
            exe,
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/cross-engine/preflight-only"),
        ));
        let selected =
            adv_runtime::vorbis::select_policy_with_reason(&files, exe, Policy::Native).unwrap();
        eprintln!("{directory}/{exe}: {selected:?}");
    }
}

#[test]
fn standard_vorbis_callbacks_seek_lifetime_and_pcm_match_reference() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = root.join("target/vorbis-codec-tests");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build.ps1"))
        .arg("-OutputDirectory")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let originals = sample(NANA);
    let native_available = originals.as_ref().is_some_and(|originals| {
        ["vorbisfile.dll", "vorbis.dll", "ogg.dll"]
            .iter()
            .all(|name| originals.join(name).exists())
    });
    if let Some(originals) = originals.filter(|_| native_available) {
        for name in ["vorbisfile.dll", "vorbis.dll", "ogg.dll"] {
            std::fs::copy(originals.join(name), output.join(name)).unwrap();
        }
    }
    for (tone, exe) in [
        ("mono", "fixture_vorbis_codec.exe"),
        ("stereo", "fixture_vorbis_codec_stereo.exe"),
    ] {
        let data = root.join("crates/adv-runtime/tests/data");
        std::fs::copy(
            data.join(format!("tone-{tone}.ogg")),
            output.join("fixture_codec.ogg"),
        )
        .unwrap();
        let reference = std::fs::read(data.join(format!("tone-{tone}.pcm"))).unwrap();
        if native_available {
            let status = Command::new(output.join(exe))
                .current_dir(&output)
                .status()
                .unwrap();
            assert_eq!(status.code(), Some(0), "native callback fixture {tone}");
            let pcm = std::fs::read(output.join("output.pcm")).unwrap();
            assert!(pcm.len() >= reference.len());
            assert!(
                snr(&reference, &pcm) > 60.0,
                "native legacy Vorbis PCM differs for {tone}"
            );
        }
        for (policy, jit) in [
            (Policy::Native, "interpreter"),
            (Policy::Native, "jit"),
            (Policy::Native, "unicorn"),
            (Policy::Guest, "interpreter"),
            (Policy::Guest, "jit"),
            (Policy::Guest, "unicorn"),
        ] {
            if policy == Policy::Guest && !native_available {
                continue;
            }
            let profile = output.join(format!("{tone}-{policy:?}-{jit}"));
            let mut runtime = Runtime::with_cpu_and_codec(
                output.join(exe),
                &profile,
                cpu::Cpu::new(jit).unwrap(),
                32,
                32,
                policy,
            )
            .unwrap();
            runtime.machine.sys.clock = Clock::virtual_time();
            runtime
                .machine
                .set_execution_budget(Some(Duration::from_secs(10)));
            let deadline = Instant::now() + Duration::from_secs(15);
            loop {
                assert!(
                    Instant::now() < deadline,
                    "codec fixture wall deadline {tone}/{policy:?}/{jit}"
                );
                match runtime.step_for(Duration::from_millis(8)) {
                    SliceResult::Yielded => {}
                    SliceResult::Complete(RunResult::Idle {
                        next_deadline: Some(at),
                    }) => runtime.machine.sys.clock.advance_to(at),
                    SliceResult::Complete(RunResult::Exited(code)) => {
                        assert_eq!(code, 0, "callback fixture {tone}/{policy:?}/{jit}");
                        break;
                    }
                    other => panic!("codec fixture {tone}/{policy:?}/{jit}: {other:?}"),
                }
            }
            let actual = std::fs::read(profile.join("overlay/output.pcm")).unwrap();
            assert!(actual.len() >= reference.len());
            if policy == Policy::Native {
                assert_eq!(
                    actual.len(),
                    11025 * 2 * if tone == "mono" { 1 } else { 2 },
                    "native host clips exactly at EOS granule"
                );
            }
            let quality = snr(&reference, &actual);
            assert!(
                quality > 60.0,
                "{tone}/{policy:?}/{jit}: PCM SNR {quality}dB"
            );
            if policy == Policy::Native {
                let channels = if tone == "mono" { 1 } else { 2 };
                let seeked = std::fs::read(profile.join("overlay/seek.pcm")).unwrap();
                let offset = 2205 * 2 * channels;
                assert_eq!(
                    seeked,
                    &actual[offset..offset + seeked.len()],
                    "PCM seek must produce the exact subsequent samples"
                );
                let stats = adv_runtime::vorbis::stats(&runtime.machine).unwrap();
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
                eprintln!("native provider: {stats:?}");
            }
            eprintln!(
                "{tone}/{policy:?}/jit={jit}: {} PCM bytes, SNR {quality:.2} dB",
                actual.len()
            );
        }
    }
}

/// Manual acceptance only: authorized unpacked inputs live under target, and
/// the original game DLL is read-only source material for a native baseline.
#[test]
#[ignore = "requires locally unpacked, authorized game sources under target"]
fn real_vorbis_pages_and_packet_boundaries_match_reference() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = root.join("target/vorbis-real-source-tests");
    let build = Command::new("powershell.exe")
        .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
        .arg(root.join("tools/fixtures/build.ps1"))
        .arg("-OutputDirectory")
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stderr)
    );
    let originals = sample(NANA).expect("the original codec DLLs under $ADV_SAMPLES");
    for name in ["vorbisfile.dll", "vorbis.dll", "ogg.dll"] {
        std::fs::copy(originals.join(name), output.join(name)).unwrap();
    }
    let sources = root.join("target/manual-regressions/audio-reference-src");
    let mut report = String::new();
    for name in [
        "Opening01_karin.ogg",
        "yayoi0006.ogg",
        "bgm02s_i.ogg",
        "bgm02s_L.ogg",
        "bgm03.ogg",
    ] {
        std::fs::copy(sources.join(name), output.join("fixture_codec.ogg")).unwrap();
        let reference = std::fs::read(sources.join(format!("{name}.s16le"))).unwrap();
        let status = Command::new(output.join("fixture_vorbis_decode.exe"))
            .current_dir(&output)
            .status()
            .unwrap();
        assert_eq!(status.code(), Some(0), "original Windows decoder {name}");
        let legacy = std::fs::read(output.join("output.pcm")).unwrap();
        assert_eq!(
            legacy.len(),
            reference.len(),
            "original Windows length {name}"
        );
        let legacy_quality = snr(&reference, &legacy);
        assert!(
            legacy_quality > 60.0,
            "legacy/reference SNR {name}: {legacy_quality}"
        );
        std::fs::write(output.join(format!("{name}.original.pcm")), &legacy).unwrap();
        for jit in ["interpreter", "jit", "unicorn"] {
            let profile = output.join(format!("{name}-native-{jit}"));
            let mut runtime = Runtime::with_cpu_and_codec(
                output.join("fixture_vorbis_decode.exe"),
                &profile,
                cpu::Cpu::new(jit).unwrap(),
                32,
                32,
                Policy::Native,
            )
            .unwrap();
            runtime.machine.sys.clock = Clock::virtual_time();
            runtime
                .machine
                .set_execution_budget(Some(Duration::from_secs(60)));
            let deadline = Instant::now() + Duration::from_secs(70);
            loop {
                assert!(
                    Instant::now() < deadline,
                    "real source wall deadline {name}/{jit}"
                );
                match runtime.step_for(Duration::from_millis(8)) {
                    SliceResult::Yielded => {}
                    SliceResult::Complete(RunResult::Idle {
                        next_deadline: Some(at),
                    }) => runtime.machine.sys.clock.advance_to(at),
                    SliceResult::Complete(RunResult::Exited(code)) => {
                        assert_eq!(code, 0, "real source {name}/{jit}");
                        break;
                    }
                    other => panic!("real source {name}/{jit}: {other:?}"),
                }
            }
            let actual = std::fs::read(profile.join("overlay/output.pcm")).unwrap();
            assert_eq!(
                actual.len(),
                reference.len(),
                "EOS granule and frame count {name}/{jit}"
            );
            let looped = std::fs::read(profile.join("overlay/loop.pcm")).unwrap();
            assert_eq!(
                looped,
                &actual[..looped.len()],
                "EOF to seek-zero must restart the exact original samples {name}/{jit}"
            );
            let metadata = std::fs::read(profile.join("overlay/metadata.bin")).unwrap();
            let channels = u32::from_le_bytes(metadata[..4].try_into().unwrap()) as usize;
            let tail = std::fs::read(profile.join("overlay/tail.pcm")).unwrap();
            assert_eq!(tail.len(), 257 * channels * 2);
            assert_eq!(
                tail,
                &actual[actual.len() - tail.len()..],
                "precise seek near EOS {name}/{jit}"
            );
            let quality = snr(&reference, &actual);
            let comparison = snr(&legacy, &actual);
            let max_error = samples(&reference)
                .into_iter()
                .zip(samples(&actual))
                .map(|(a, b)| (a - b).abs())
                .fold(0.0, f64::max);
            assert!(
                quality > 60.0 && comparison > 60.0,
                "real source SNR {name}/{jit}: reference {quality} original {comparison}"
            );
            // Windowed SNR also catches local errors at page/packet seams
            // that a whole 167-second aggregate could conceal.
            let mut min_window = f64::INFINITY;
            for (r, a) in reference.chunks(8192).zip(actual.chunks(8192)) {
                let energy = samples(r).into_iter().map(|s| s * s).sum::<f64>();
                if energy > r.len() as f64 {
                    min_window = min_window.min(snr(r, a));
                }
            }
            assert!(
                min_window > 50.0,
                "local packet/page seam SNR {name}/{jit}: {min_window}"
            );
            let stats = adv_runtime::vorbis::stats(&runtime.machine).unwrap();
            let row = format!(
                "{name} jit={jit} bytes={} reference_snr_db={quality:.3} original_snr_db={comparison:.3} legacy_reference_snr_db={legacy_quality:.3} min_window_snr_db={min_window:.3} max_lsb_error={max_error:.0} stats={stats:?}\n",
                actual.len()
            );
            eprint!("{row}");
            report.push_str(&row);
            std::fs::write(output.join("report.txt"), &report).unwrap();
        }
    }
}
