//! Owned popups show as planes over the main window's frame: the frame keeps
//! the main window's size, a popup off the main window moves into it, a
//! closed popup shows the main window again, and host points keep the main
//! window's coordinates.
#![cfg(any(target_vendor = "apple", target_os = "android"))]
#[path = "../../adv-nt/tests/common/mod.rs"]
mod common;

use adv_abi::{
    Conv, Outcome, Registry,
    dll::{Export, ModuleImage},
};
use adv_device::surface::{Snapshot, SurfaceStore};
use adv_machine::{HostFn, Resolver, RunResult};

struct Platform(Registry);
impl Resolver for Platform {
    fn host_fn(&self, module: &str, name: &str) -> Option<HostFn> {
        adv_unix::Platform
            .host_fn(module, name)
            .or_else(|| self.0.get(module, name))
    }
    fn provides(&self, module: &str) -> bool {
        adv_unix::Platform.provides(module) || self.0.provides(module)
    }
}
struct Checkpoints {
    #[cfg(feature = "gpu")]
    frames: adv_device::gpu::test_support::SurfaceCapture,
    #[cfg(not(feature = "gpu"))]
    frames: adv_device::surface::FrameConsumer,
    snapshot: Snapshot,
    /// Each checkpoint's frame: its size and pixels.
    seen: Vec<(u32, u32, Vec<u8>)>,
}

const BLUE: [u8; 3] = [255, 0, 0];
const RED: [u8; 3] = [0, 0, 255];
const GREEN: [u8; 3] = [0, 255, 0];

// spec: WINE-GFX-10, WINE-IN-1
#[test]
fn owned_popups_show_over_the_main_frame_without_replacing_it() {
    let mut registry = Registry::default();
    adv_runtime::wine::register(&mut registry);
    registry.add(
        "planes_probe.dll",
        "checkpoint",
        Conv::Cdecl { argc: 1 },
        |env| {
            if env.arg(0) == 1 {
                adv_drv::user::input::post(env.ext, 0x200, 0, 10 << 16 | 20).unwrap();
                adv_nt::server::run(env);
            }
            let (checkpoints, store) = env.ext.get2_installed_mut::<Checkpoints, SurfaceStore>();
            #[cfg(feature = "gpu")]
            {
                let before = store.presenter.frames;
                checkpoints.frames.update(store);
                let frame = &store.presenter;
                if frame.frames != before {
                    let snapshot = &mut checkpoints.snapshot;
                    (snapshot.width, snapshot.height) = (frame.width, frame.height);
                    snapshot.bgra.clone_from(&frame.bgra);
                }
            }
            #[cfg(not(feature = "gpu"))]
            {
                let _ = store;
                if checkpoints.frames.update() {
                    let frame = checkpoints.frames.current();
                    frame.snapshot(&mut checkpoints.snapshot).unwrap();
                }
            }
            let snapshot = &checkpoints.snapshot;
            let frame = (snapshot.width, snapshot.height, snapshot.bgra.clone());
            checkpoints.seen.push(frame);
            Outcome::Return(0)
        },
    );
    let mut seen = Vec::new();
    #[cfg(feature = "gpu")]
    let mut capture = Some(adv_device::gpu::test_support::SurfaceCapture::new().unwrap());
    let Some((result, output)) = common::run_probe_inspect(
        "window_planes.exe",
        false,
        adv_runtime::wine::GRAPHICS_DRIVER,
        Platform(registry),
        |machine, prefix| {
            adv_unix::install(machine);
            adv_runtime::wine::install(machine, &prefix.join("drive_c"), 640, 480).unwrap();
            let image = ModuleImage::build(
                machine,
                "planes_probe.dll",
                &[Export {
                    name: "checkpoint",
                    ordinal: 1,
                }],
                &[],
            )
            .unwrap();
            adv_unix::builtin::install(&prefix.join("drive_c"), &image).unwrap();
            #[cfg(feature = "gpu")]
            let frames = capture.take().unwrap();
            #[cfg(not(feature = "gpu"))]
            let frames = {
                let (producer, frames) = adv_device::surface::frame_mailbox();
                machine
                    .ext
                    .get_mut::<SurfaceStore>()
                    .set_frame_mailbox(producer, adv_sync::WakeEvent::new().unwrap());
                frames
            };
            machine.ext.insert(Checkpoints {
                frames,
                snapshot: Snapshot::default(),
                seen: Vec::new(),
            });
        },
        |machine| seen.clone_from(&machine.ext.get::<Checkpoints>().seen),
    ) else {
        return;
    };
    assert!(
        matches!(result, RunResult::Exited(0)),
        "{result:?}: {output}"
    );
    assert_eq!(seen.len(), 4);
    // Popups in main-window pixels: the red one at (16, 8), and the green
    // one, shown far outside, moved to the bottom right corner.
    let red = |x, y| (16..48).contains(&x) && (8..24).contains(&y);
    let green = |x, y| (112..128).contains(&x) && (48..64).contains(&y);
    for (stage, shows_red, shows_green) in [
        (0, false, false),
        (1, true, false),
        (2, true, true),
        (3, false, true),
    ] {
        let (width, height, bgra) = &seen[stage];
        assert_eq!((*width, *height), (128, 64), "stage {stage}");
        for y in 0..64 {
            for x in 0..128 {
                let at = (y * 128 + x) * 4;
                let expected = if shows_green && green(x, y) {
                    GREEN
                } else if shows_red && red(x, y) {
                    RED
                } else {
                    BLUE
                };
                assert_eq!(bgra[at..at + 3], expected, "stage {stage} at {x},{y}");
            }
        }
    }
}
