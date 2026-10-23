//! GDI paints a window on the Wine userland: our driver gives the window a
//! surface over store pixels, and its flush reaches the presenter.
#![cfg(target_vendor = "apple")]

#[path = "../../adv-nt/tests/common/mod.rs"]
mod common;

use adv_abi::Registry;
use adv_device::surface::SurfaceStore;
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

/// Pixels of `color` (BGRA, alpha ignored) in the top-left `width` × `height`
/// of a frame `stride` pixels wide.
fn count(bgra: &[u8], stride: usize, width: usize, height: usize, color: [u8; 3]) -> usize {
    (0..height)
        .flat_map(|y| (0..width).map(move |x| (y * stride + x) * 4))
        .filter(|&at| bgra[at..at + 3] == color)
        .count()
}

// spec: WINE-GFX-4, WINE-GFX-5
#[test]
fn gdi_painting_a_window_reaches_the_presenter() {
    let mut registry = Registry::default();
    adv_runtime::wine::register(&mut registry);
    let mut presented = None;
    let Some((result, output)) = common::run_probe_inspect(
        "window.exe",
        false,
        adv_runtime::wine::GRAPHICS_DRIVER,
        Platform(registry),
        |machine, prefix| {
            adv_unix::install(machine);
            adv_runtime::wine::install(machine, prefix, 640, 480).unwrap();
        },
        |machine| {
            let frame = &machine.ext.get::<SurfaceStore>().presenter;
            let stride = frame.width as usize;
            presented = Some((
                (frame.width, frame.height),
                frame.frames,
                count(&frame.bgra, stride, 64, 32, [0xff, 0xff, 0xff]),
                count(&frame.bgra, stride, 64, 32, [0xff, 0, 0]),
            ));
        },
    ) else {
        return;
    };
    assert!(
        matches!(result, RunResult::Exited(0)),
        "{result:?}: {output}"
    );
    // The same line as the probe prints on Wine.
    assert_eq!(
        output.lines().collect::<Vec<_>>(),
        ["painted=1 inked=43 background=2005 other=0"]
    );
    let (size, frames, inked, background) = presented.unwrap();
    assert!(frames >= 1, "no frame was presented");
    assert_eq!(size, (64, 32));
    assert_eq!((inked, background), (43, 2005));
}
