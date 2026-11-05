//! Wine's window surface flush preserves damage and suppresses identical redraws.
#![cfg(any(target_vendor = "apple", target_os = "android"))]
#[path = "../../adv-nt/tests/common/mod.rs"]
mod common;

use adv_abi::{
    Conv, Outcome, Registry,
    dll::{Export, ModuleImage},
};
use adv_device::surface::{Rect, Snapshot, SurfaceStore};
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
    /// Software frames reach the native display in GPU builds and the frame
    /// mailbox otherwise.
    #[cfg(feature = "gpu")]
    frames: adv_device::gpu::test_support::SurfaceCapture,
    #[cfg(not(feature = "gpu"))]
    frames: adv_device::surface::FrameConsumer,
    snapshot: Snapshot,
    seen: Vec<(u32, u64, u64, bool, Vec<Rect>)>,
}

// spec: DEV-FRAME-3, DEV-FRAME-5, DEV-FRAME-10, DEV-FRAME-12, HOST-DAMAGE-1, WINE-IN-1, WINE-IN-3
#[test]
fn wine_identical_redraws_publish_nothing_and_partial_redraws_keep_damage() {
    let mut registry = Registry::default();
    adv_runtime::wine::register(&mut registry);
    registry.add(
        "frame_probe.dll",
        "checkpoint",
        Conv::Cdecl { argc: 1 },
        |env| {
            let stage = env.arg(0);
            if stage == 4 {
                adv_drv::user::input::post(env.ext, 0x200, 0, 17 << 16 | 13).unwrap();
                adv_nt::server::run(env);
            }
            let (checkpoints, store) = env.ext.get2_installed_mut::<Checkpoints, SurfaceStore>();
            let (published, copied) = (store.stats.published_frames, store.stats.copied_bytes);
            #[cfg(feature = "gpu")]
            let (updated, damage) = {
                let before = store.presenter.frames;
                checkpoints.frames.update(store);
                let frame = &store.presenter;
                let updated = frame.frames != before;
                if updated {
                    let snapshot = &mut checkpoints.snapshot;
                    (snapshot.width, snapshot.height) = (frame.width, frame.height);
                    snapshot.bgra.clone_from(&frame.bgra);
                }
                let damage = if updated {
                    adv_device::surface::rects(&frame.damage)
                } else {
                    Vec::new()
                };
                (updated, damage)
            };
            #[cfg(not(feature = "gpu"))]
            let (updated, damage) = {
                let updated = checkpoints.frames.update();
                let damage = if updated {
                    let frame = checkpoints.frames.current();
                    frame.snapshot(&mut checkpoints.snapshot).unwrap();
                    frame.damage()
                } else {
                    Vec::new()
                };
                (updated, damage)
            };
            checkpoints
                .seen
                .push((stage, published, copied, updated, damage));
            Outcome::Return(0)
        },
    );
    let mut seen = Vec::new();
    // The capture precedes the store's GPU facade on this thread.
    #[cfg(feature = "gpu")]
    let mut capture = Some(adv_device::gpu::test_support::SurfaceCapture::new().unwrap());
    let Some((result, output)) = common::run_probe_inspect(
        "frame_damage.exe",
        false,
        adv_runtime::wine::GRAPHICS_DRIVER,
        Platform(registry),
        |machine, prefix| {
            adv_unix::install(machine);
            adv_runtime::wine::install(machine, &prefix.join("drive_c"), 640, 480).unwrap();
            let image = ModuleImage::build(
                machine,
                "frame_probe.dll",
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
        |machine| {
            let checkpoints = machine.ext.get::<Checkpoints>();
            seen.clone_from(&checkpoints.seen);
            let snapshot = &checkpoints.snapshot;
            assert_eq!((snapshot.width, snapshot.height), (128, 64));
            for y in 0..64 {
                for x in 0..128 {
                    let at = (y * 128 + x) * 4;
                    let expected = if (0..16).contains(&x) && (5..8).contains(&y) {
                        [0, 0, 255]
                    } else {
                        [255, 0, 0]
                    };
                    assert_eq!(snapshot.bgra[at..at + 3], expected);
                }
            }
        },
    ) else {
        return;
    };
    assert!(
        matches!(result, RunResult::Exited(0)),
        "{result:?}: {output}"
    );
    assert_eq!(seen.len(), 5, "{seen:?}");
    assert!(seen[0].3);
    assert_eq!(
        (seen[1].1, seen[1].2, seen[1].3),
        (seen[0].1, seen[0].2, false)
    );
    assert_eq!(seen[2].1, seen[1].1 + 1);
    assert!(seen[2].3);
    assert_eq!(
        seen[2].4,
        // Rows 5..8 of the first 16 columns, in 8-pixel cells.
        [Rect {
            x: 0,
            y: 0,
            w: 16,
            h: 8
        }]
    );
    assert_eq!(
        (seen[3].1, seen[3].2, seen[3].3),
        (seen[2].1, seen[2].2, false)
    );
    assert_eq!(
        (seen[4].1, seen[4].2, seen[4].3),
        (seen[3].1, seen[3].2, false)
    );
}
