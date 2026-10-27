//! Our Direct3D 9 on the Wine userland: Wine's GDI draws text into an
//! offscreen plain surface through the surface's DC, and the surface copied to
//! the back buffer and presented reaches the presenter.
#![cfg(all(any(target_vendor = "apple", target_os = "android"), feature = "gpu"))]

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

// spec: WINE-GFX-6
#[test]
fn wine_gdi_draws_into_d3d9_surfaces_that_reach_the_presenter() {
    let mut registry = Registry::default();
    adv_runtime::wine::register(&mut registry);
    let mut presented = None;
    let Some((result, output)) = common::run_probe_inspect(
        "d3d9.exe",
        false,
        adv_runtime::wine::GRAPHICS_DRIVER,
        Platform(registry),
        |machine, prefix| {
            adv_unix::install(machine);
            adv_runtime::wine::install(machine, prefix, 640, 480).unwrap();
            // As the shell attaches a GPU: its wake, and the readback worker.
            let gpu = block_on(adv_device::gpu::Gpu::request(false)).unwrap();
            let wake = adv_sync::WakeEvent::new().unwrap();
            assert!(gpu.context().set_execution_wake(wake.clone()).is_ok());
            machine.cpu.set_waker(wake.clone()).unwrap();
            machine
                .ext
                .get_mut::<SurfaceStore>()
                .set_deferred_presentation(true)
                .unwrap();
            adv_dx::d3d9::set_gpu(&mut machine.ext, gpu).unwrap();
            let sender = machine.cpu.background_sender().unwrap();
            adv_dx::d3d9::set_background(&mut machine.ext, sender, wake.clone());
            machine.ext.insert(Wake(wake));
            machine.ext.insert(common::HostServices(serve));
        },
        |machine| {
            capture(machine);
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
    // Every call succeeds, and Wine's GDI inks the text as in the other
    // probes; Wine's own Direct3D 9 needs OpenGL, so it has no line here.
    assert_eq!(
        output.lines().collect::<Vec<_>>(),
        [
            "drawn=0x1 released=0 pitch=256 inked=43 background=2005 other=0 \
             backdrawn=0x1 backreleased=0 presented=0"
        ]
    );
    let (size, frames, inked, background) = presented.unwrap();
    assert!(frames >= 1, "no frame was presented");
    // The device presents its 64 × 32 back buffer to the probe window.
    assert_eq!(size, (64, 32));
    assert_eq!((inked, background), (43, 2005));
}

/// Read the latest native frame back into the presenter, as a shell's
/// diagnostic capture does.
fn capture(machine: &mut adv_machine::Machine<adv_cpu::Cpu>) {
    let Some(composition) = machine
        .ext
        .get::<SurfaceStore>()
        .latest_frame()
        .filter(|frame| frame.native().is_some())
        .cloned()
    else {
        return;
    };
    let image = composition.native().unwrap().image.clone();
    let mut readback = adv_dx::d3d9::readback_image(&mut machine.ext, &image).unwrap();
    if let Some(job) = readback.take_completion_job() {
        job();
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let bgra = loop {
        if let Some(pixels) = readback.poll() {
            break pixels.unwrap();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "bounded frame readback"
        );
        std::thread::sleep(std::time::Duration::from_millis(1));
    };
    machine
        .ext
        .get_mut::<SurfaceStore>()
        .capture_native_frame_pixels(&composition, &bgra)
        .unwrap();
}

/// The machine's wake, which the GPU and its readback worker signal.
struct Wake(adv_sync::WakeEvent);

/// What the runtime's step does for Direct3D 9 while the guest waits: take
/// finished GPU work, waiting a bounded time for it.
fn serve(machine: &mut adv_machine::Machine<adv_cpu::Cpu>) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if adv_dx::d3d9::pump(machine).unwrap() {
            return true;
        }
        let wake = &machine.ext.get::<Wake>().0;
        wake.wait(Some(std::time::Duration::from_millis(10)))
            .unwrap();
    }
    false
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    struct Wake(std::thread::Thread);
    impl std::task::Wake for Wake {
        fn wake(self: std::sync::Arc<Self>) {
            self.0.unpark();
        }
    }
    let waker = std::task::Waker::from(std::sync::Arc::new(Wake(std::thread::current())));
    let mut context = std::task::Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let std::task::Poll::Ready(value) = future.as_mut().poll(&mut context) {
            return value;
        }
        std::thread::park();
    }
}
