//! Silent x86 GDI raster fixture: exact native output through all guest backends.
#![cfg(windows)]
use adv_machine::{Clock, RunResult, SliceResult};
use adv_runtime::{RunMode, boot::Runtime};
use pipeline::cpu;
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::OnceLock,
    time::{Duration, Instant},
};
#[path = "../examples/support/pipeline.rs"]
mod pipeline;

fn call(f: &mut pipeline::Fixture, name: &str, args: &[u32]) -> u32 {
    let address = f.machine.resolve_import("gdi32.dll", name).unwrap();
    f.entries.insert(9000, address);
    pipeline::invoke(f, 9000, args)
}

#[test]
fn text_uses_shared_gdi_directdraw_surface_and_releases_only_the_borrowed_dc() {
    use adv_cpu::backend::{Guest, GuestExt};
    for ddraw in [false, true] {
        let mut f = pipeline::fixture(ddraw, 64, 32, false);
        f.run().unwrap();
        let out = pipeline::DATA + 0xa00;
        f.machine.cpu.write(out, b"Glyph").unwrap();
        let source = if ddraw {
            pipeline::SURFACE_OUT
        } else {
            pipeline::BITMAP_OUT
        };
        let surface = f.machine.cpu.read_u32(source).unwrap();
        let dc = if ddraw {
            assert_eq!(
                pipeline::invoke(&mut f, adv_runtime::api::SURFACE_DC, &[surface, out + 64]),
                0
            );
            f.machine.cpu.read_u32(out + 64).unwrap()
        } else {
            f.machine.cpu.read_u32(pipeline::DC_OUT).unwrap()
        };
        let sid = match f
            .machine
            .ext
            .get::<adv_win32::Win32>()
            .gdi
            .get(&f.machine.sys.objects, dc)
        {
            Some(adv_os::gdi::GdiObj::Dc { selected, .. }) => selected[3],
            _ => panic!("missing shared DC"),
        };
        let s = f.graphics().get(sid).unwrap().clone();
        let before = f.read(s.pixels, (s.pitch * s.height) as usize);
        let frames = f.graphics().stats.presentations;
        call(&mut f, "SetBkMode", &[dc, 1]);
        call(&mut f, "SetTextColor", &[dc, 0x004080f0]);
        assert_eq!(call(&mut f, "TextOutA", &[dc, 2, 3, out, 5]), 1);
        assert_ne!(f.read(s.pixels, (s.pitch * s.height) as usize), before);
        assert_eq!(
            f.graphics().stats.presentations,
            frames,
            "memory/borrowed DC cannot present early"
        );
        assert_eq!(f.machine.cpu.read_u32(source).unwrap(), surface);
        if ddraw {
            let objects = f.machine.sys.objects.object_count();
            assert_eq!(
                pipeline::invoke(&mut f, adv_runtime::api::SURFACE_RELEASE_DC, &[surface, dc]),
                0
            );
            assert_eq!(f.machine.sys.objects.object_count(), objects - 1);
            assert_eq!(call(&mut f, "TextOutA", &[dc, 2, 3, out, 5]), 0);
            assert!(f.graphics().get(sid).is_some());
        }
    }
}

#[test]
fn invalid_flags_ranges_and_readonly_pixels_fail_without_publishing_or_leaking() {
    use adv_cpu::backend::{Guest, GuestExt, Prot};
    let mut f = pipeline::fixture(false, 64, 32, false);
    f.run().unwrap();
    let dc = f.machine.cpu.read_u32(pipeline::DC_OUT).unwrap();
    let id = f.machine.cpu.read_u32(pipeline::BITMAP_OUT).unwrap();
    let s = f.graphics().get(id).unwrap().clone();
    let input = pipeline::DATA + 0xa00;
    f.machine.cpu.write(input, b"AV").unwrap();
    let before = f.read(s.pixels, (s.pitch * s.height) as usize);
    let version = s.version;
    let objects = f.machine.sys.objects.object_count();
    for _ in 0..8 {
        assert_eq!(call(&mut f, "TextOutA", &[dc, 0, 0, u32::MAX - 1, 4]), 0);
        assert_eq!(call(&mut f, "TextOutW", &[dc, 0, 0, input, u32::MAX]), 0);
        assert_eq!(
            call(&mut f, "ExtTextOutA", &[dc, 0, 0, 0x10, 0, input, 2, 0]),
            0
        );
        assert_eq!(
            call(&mut f, "ExtTextOutA", &[dc, 0, 0, 0, 0, input, 2, input]),
            0
        );
        assert_eq!(
            call(
                &mut f,
                "ExtTextOutW",
                &[dc, 0, 0, 4, u32::MAX - 7, input, 2, 0]
            ),
            0
        );
    }
    assert_eq!(call(&mut f, "SetTextAlign", &[dc, 1]), u32::MAX);
    assert_eq!(call(&mut f, "GetTextAlign", &[dc]), 0);
    let page = s.pixels & !4095;
    let end = (s.pixels + s.pitch * s.height + 4095) & !4095;
    f.machine.cpu.protect(page, end - page, Prot::READ).unwrap();
    assert_eq!(call(&mut f, "TextOutA", &[dc, 0, 0, input, 2]), 0);
    f.machine.cpu.protect(page, end - page, Prot::RW).unwrap();
    assert_eq!(f.read(s.pixels, (s.pitch * s.height) as usize), before);
    assert_eq!(f.graphics().get(id).unwrap().version, version);
    assert_eq!(f.machine.sys.objects.object_count(), objects);
    assert_eq!(adv_win32::gdi32::font::cache_stats(&f.machine).fonts, 0);
    f.machine
        .cpu
        .protect(pipeline::INPUT_A, 4096, Prot::WRITE)
        .unwrap();
    assert_eq!(
        call(&mut f, "TextOutA", &[dc, 0, 0, pipeline::INPUT_A, 2]),
        0
    );
    f.machine
        .cpu
        .protect(pipeline::INPUT_A, 4096, Prot::RW)
        .unwrap();
    assert_eq!(adv_win32::gdi32::font::cache_stats(&f.machine).fonts, 0);
    // A clipped-out or empty run is valid and does not publish a frame/version.
    assert_eq!(
        call(&mut f, "TextOutA", &[dc, u32::MAX / 2, 0, input, 2]),
        1
    );
    assert_eq!(call(&mut f, "TextOutW", &[dc, 0, 0, 0, 0]), 1);
    assert_eq!(f.graphics().get(id).unwrap().version, version);
}

fn fixtures() -> &'static Path {
    static DIRECTORY: OnceLock<PathBuf> = OnceLock::new();
    DIRECTORY.get_or_init(|| {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let directory = root.join("target/gdi-text-paint-tests");
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(root.join("tools/fixtures/build-gdi-text-paint.ps1"))
            .arg("-OutputDirectory")
            .arg(&directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        directory
    })
}
fn run(runtime: &mut Runtime<cpu::Cpu>) -> RunResult {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        assert!(
            Instant::now() < deadline,
            "paint fixture exceeded its wall-clock bound"
        );
        match runtime.step_for(Duration::from_millis(4)) {
            SliceResult::Yielded => {}
            SliceResult::Complete(RunResult::Idle {
                next_deadline: Some(at),
            }) => runtime.machine.sys.clock.advance_to(at),
            SliceResult::Complete(result) => return result,
        }
    }
}
fn runtime(directory: &Path, executable: &str, backend: &str, suffix: &str) -> Runtime<cpu::Cpu> {
    let mut runtime = Runtime::with_cpu(
        directory.join(executable),
        directory.join(suffix),
        cpu::Cpu::new(backend).unwrap(),
        64,
        32,
    )
    .unwrap();
    runtime.set_api_mode(RunMode::Strict);
    runtime.machine.sys.clock = Clock::virtual_time();
    runtime
        .machine
        .set_execution_budget(Some(Duration::from_secs(20)));
    runtime
}
#[test]
fn typed_output_matches_native_across_formats_and_shared_dc_selections() {
    use std::os::windows::process::CommandExt;
    let directory = fixtures();
    let output = directory.join("gdi-text-paint.bin");
    if output.exists() {
        std::fs::remove_file(&output).unwrap();
    }
    let mut native = Command::new(directory.join("fixture_gdi_text_paint.exe"))
        .current_dir(directory)
        .creation_flags(0x08000000)
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = native.try_wait().unwrap() {
            assert_eq!(status.code(), Some(0));
            break;
        }
        if Instant::now() >= deadline {
            native.kill().unwrap();
            native.wait().unwrap();
            panic!("owned native paint timed out");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let reference = std::fs::read(&output).unwrap();
    assert!(reference.len() > 2_000_000);
    for backend in ["unicorn", "interpreter", "jit"] {
        let suffix = format!("paint-{backend}");
        let path = directory.join(&suffix).join("overlay/gdi-text-paint.bin");
        if path.exists() {
            std::fs::remove_file(&path).unwrap();
        }
        let mut runtime = runtime(directory, "fixture_gdi_text_paint.exe", backend, &suffix);
        let result = run(&mut runtime);
        assert!(
            matches!(result, RunResult::Exited(0)),
            "{backend}: {result:?}"
        );
        assert!(
            runtime.api_acceptance_eligible(),
            "{backend}: no stub permitted"
        );
        let actual = std::fs::read(path).unwrap();
        assert_eq!(actual.len(), reference.len());
        if let Some(i) = actual.iter().zip(&reference).position(|(a, b)| a != b) {
            panic!(
                "{backend}: native byte {i}: {} != {}",
                actual[i], reference[i]
            );
        }
        assert_eq!(
            adv_win32::gdi32::font::cache_stats(&runtime.machine).fonts,
            4
        );
    }
}
