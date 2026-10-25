//! Wine's window surface flush preserves damage and suppresses identical redraws.
#![cfg(any(target_vendor = "apple", target_os = "android"))]
#[path = "../../adv-nt/tests/common/mod.rs"]
mod common;

use adv_abi::{
    Conv, Outcome, Registry,
    dll::{Export, ModuleImage},
};
use adv_device::surface::{FrameConsumer, Rect, Snapshot, SurfaceStore, frame_mailbox};
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
    frames: FrameConsumer,
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
            let store = env.ext.get::<SurfaceStore>();
            let (published, copied) = (store.stats.published_frames, store.stats.copied_bytes);
            let checkpoints = env.ext.get_mut::<Checkpoints>();
            let updated = checkpoints.frames.update();
            let damage = if updated {
                checkpoints
                    .frames
                    .current()
                    .snapshot(&mut checkpoints.snapshot)
                    .unwrap();
                checkpoints.frames.current().damage().to_vec()
            } else {
                Vec::new()
            };
            checkpoints
                .seen
                .push((stage, published, copied, updated, damage));
            Outcome::Return(0)
        },
    );
    let mut seen = Vec::new();
    let Some((result, output)) = common::run_probe_inspect(
        "frame_damage.exe",
        false,
        adv_runtime::wine::GRAPHICS_DRIVER,
        Platform(registry),
        |machine, prefix| {
            adv_unix::install(machine);
            adv_runtime::wine::install(machine, prefix, 640, 480).unwrap();
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
            adv_unix::builtin::install(prefix, &image).unwrap();
            let (producer, frames) = frame_mailbox();
            machine
                .ext
                .get_mut::<SurfaceStore>()
                .set_frame_mailbox(producer, adv_sync::WakeEvent::new().unwrap());
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
        [Rect {
            x: 0,
            y: 5,
            w: 16,
            h: 3
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
