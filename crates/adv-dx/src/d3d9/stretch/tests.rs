//! Authored color blits over one shared GPU and one readback worker.
//! Native x86 oracle: DEFAULT 21 -> backbuffer 22, offset 2x2 rectangles,
//! filters NONE/POINT/LINEAR, both inside/outside BeginScene, all S_OK.
//! X in X8R8G8B8 is unused; native comparisons concern RGB, not that byte.
use super::*;
use adv_abi::{
    ApiDiagnostics,
    types::{D3DLOCKED_RECT, D3DSURFACE_DESC},
};
use adv_cpu::{
    JitCpu,
    backend::{Guest, Reg},
};
use adv_device::gpu::Image;
use adv_machine::{Resume, WakeReason};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

const STACK: u32 = 0x550000;
#[derive(Default)]
struct Surfaces {
    next: u32,
    targets: BTreeMap<u32, Target>,
}
fn env(m: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn parameters() -> D3DPRESENT_PARAMETERS {
    D3DPRESENT_PARAMETERS {
        BackBufferWidth: 16,
        BackBufferHeight: 12,
        BackBufferFormat: 22,
        BackBufferCount: 1,
        SwapEffect: 1,
        hDeviceWindow: 0x7788,
        Windowed: 1,
        ..D3DPRESENT_PARAMETERS::default()
    }
}
pub(in crate::d3d9) struct Fixture {
    pub(in crate::d3d9) m: Machine<JitCpu>,
    pub(in crate::d3d9) device: u32,
    pub(in crate::d3d9) factory: u32,
    pub(in crate::d3d9) back: u32,
    pub(in crate::d3d9) wake: Arc<adv_sync::WakeEvent>,
    _service: adv_sync::background::Service,
}
impl Fixture {
    pub(in crate::d3d9) fn new() -> Self {
        Self::with_limits(adv_machine::ObjectLimits::default())
    }
    pub(in crate::d3d9) fn with_limits(limits: adv_machine::ObjectLimits) -> Self {
        Self::with_native(limits, false)
    }
    pub(in crate::d3d9) fn with_native(limits: adv_machine::ObjectLimits, native: bool) -> Self {
        let mut m = super::super::tests::machine_with_native(native);
        // The ownership-only base deliberately limits references to three.
        // This fixture exercises two live DEFAULT resources and a device wait.
        m.sys.objects = Objects::with_limits(limits);
        m.ext.get_mut::<State>().com = Com::new(&mut m.sys.objects).unwrap();
        m.cpu.map(STACK, 4096, Prot::RW).unwrap();
        m.cpu.map(0x400000, 4096, Prot::RX).unwrap();
        m.spawn("authored color blits", 0x400000, &[], 65536)
            .unwrap();
        let gpu = pollster::block_on(Gpu::request(!cfg!(any(
            target_os = "macos",
            target_os = "android"
        ))))
        .unwrap();
        let wake = Arc::new(adv_sync::WakeEvent::new().unwrap());
        let mut builder = adv_sync::background::Builder::new((*wake).clone()).unwrap();
        let sender = builder.sender(8, 8).unwrap();
        let service = builder.start().unwrap();
        m.ext.insert(Surfaces::default());
        let state = m.ext.get_mut::<State>();
        state.gpu = Some(gpu);
        state.sender = Some(std::rc::Rc::new(std::cell::RefCell::new(sender)));
        state.wake = Some(wake.clone());
        state.provider.insert = |ext, target| {
            let s = ext.get_mut::<Surfaces>();
            s.next += 1;
            s.targets.insert(s.next, target);
            Ok(s.next)
        };
        state.provider.target = |ext, surface| ext.get::<Surfaces>().targets.get(&surface).cloned();
        state.provider.replace = |ext, surface, target| {
            *ext.get_mut::<Surfaces>().targets.get_mut(&surface).unwrap() = target;
            Ok(())
        };
        state.provider.remove = |ext, _, surface| {
            ext.get_mut::<Surfaces>().targets.remove(&surface).unwrap();
            Ok(())
        };
        let (factory, _) = object(
            &mut env(&mut m),
            "IDirect3D9",
            IID_FACTORY,
            Payload::Factory,
        )
        .unwrap();
        let mut f = Self {
            m,
            device: 0,
            factory,
            back: 0,
            wake,
            _service: service,
        };
        let input = f.packet(parameters());
        let output = f.packet(0u32);
        let result = f.call(
            "IDirect3D9::CreateDevice",
            &[factory, 0, 1, 0x7788, 0x22, input, output],
        );
        assert_eq!(f.finish(result), 0);
        f.device = f.m.cpu.read_u32(output).unwrap();
        let e = env(&mut f.m);
        let id = device(&e, f.device).unwrap().back_buffer.unwrap();
        let Some(Payload::Resource(r)) = com(&e).get_identity(&e.sys.objects, id) else {
            panic!()
        };
        f.back = r.pointer;
        f
    }
    pub(in crate::d3d9) fn packet<T: bytemuck::Pod>(&mut self, value: T) -> u32 {
        let mut e = env(&mut self.m);
        let address = alloc(&mut e, std::mem::size_of::<T>() as u32).unwrap();
        GuestPtr::new(address)
            .write(&mut self.m.cpu, value)
            .unwrap();
        address
    }
    pub(in crate::d3d9) fn call(&mut self, name: &str, args: &[u32]) -> Outcome {
        let mut registry = Registry::default();
        super::super::register(&mut registry);
        self.m.cpu.set_reg(Reg::Esp, STACK);
        self.m.cpu.write_u32(STACK, 0).unwrap();
        for (index, value) in args.iter().enumerate() {
            self.m
                .cpu
                .write_u32(STACK + 4 + index as u32 * 4, *value)
                .unwrap();
        }
        (registry.get("d3d9.dll", name).unwrap().handler)(&mut env(&mut self.m))
    }
    pub(in crate::d3d9) fn finish(&mut self, outcome: Outcome) -> u32 {
        let outcome = match outcome {
            Outcome::Block {
                wait: Wait::Object { handle, .. },
                then,
                ..
            } => {
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    super::super::pump(&mut self.m).unwrap();
                    if matches!(
                        self.m.sys.objects.get(handle),
                        Some(Object::Event { signaled: true, .. })
                    ) {
                        break;
                    }
                    assert!(Instant::now() < deadline, "bounded readback wait");
                    self.wake.wait(Some(Duration::from_millis(10))).unwrap();
                }
                then(&mut env(&mut self.m), Resume::Woke(WakeReason::Signaled))
            }
            outcome => outcome,
        };
        if matches!(outcome, Outcome::Block { .. }) {
            return self.finish(outcome);
        }
        let Outcome::Return(result) = outcome else {
            panic!("expected command completion")
        };
        result
    }
    pub(in crate::d3d9) fn okay(&mut self, name: &str, args: &[u32]) {
        let outcome = self.call(name, args);
        assert_eq!(self.finish(outcome), 0, "{name}");
    }
    pub(in crate::d3d9) fn surface(
        &mut self,
        width: u32,
        height: u32,
        format: u32,
        pool: u32,
    ) -> u32 {
        let output = self.packet(0u32);
        self.okay(
            "IDirect3DDevice9::CreateOffscreenPlainSurface",
            &[self.device, width, height, format, pool, output, 0],
        );
        self.m.cpu.read_u32(output).unwrap()
    }
    pub(in crate::d3d9) fn render_target(&mut self, width: u32, height: u32, format: u32) -> u32 {
        let output = self.packet(0u32);
        self.okay(
            "IDirect3DDevice9::CreateRenderTarget",
            &[self.device, width, height, format, 0, 0, 0, output, 0],
        );
        self.m.cpu.read_u32(output).unwrap()
    }
    pub(in crate::d3d9) fn lock(&mut self, surface: u32, rect: u32, flags: u32) -> D3DLOCKED_RECT {
        let output = self.packet(D3DLOCKED_RECT::default());
        self.okay(
            "IDirect3DSurface9::LockRect",
            &[surface, output, rect, flags],
        );
        GuestPtr::new(output).read(&self.m.cpu).unwrap()
    }
    pub(in crate::d3d9) fn image(&mut self, surface: u32) -> Image {
        self.target(surface).image
    }
    pub(in crate::d3d9) fn target(&mut self, surface: u32) -> Target {
        let e = env(&mut self.m);
        let r = resources::resource(&e, surface).unwrap();
        (e.ext.get::<State>().provider.target)(e.ext, r.surface.unwrap()).unwrap()
    }
    pub(in crate::d3d9) fn pixels(&mut self, image: &Image) -> Vec<u8> {
        let mut e = env(&mut self.m);
        let owner = com(&e)
            .identity::<Payload>(&e.sys.objects, self.device)
            .unwrap();
        let outcome = recording::barrier(&mut e, owner, recording::Reason::Readback, |_| {
            Outcome::Return(0)
        });
        assert_eq!(self.finish(outcome), 0);
        let mut transfer = self
            .m
            .ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .readback(image, None)
            .unwrap();
        transfer.take_completion_job().unwrap()();
        transfer.poll().unwrap().unwrap()
    }
    pub(in crate::d3d9) fn submissions(&self) -> u64 {
        self.m
            .ext
            .get::<State>()
            .gpu
            .as_ref()
            .unwrap()
            .stats()
            .submissions
    }
}

#[test]
fn default_cpu_staging_blits_rgb_without_blending_and_preserves_outside_pixels() {
    let mut f = Fixture::new();
    let source = f.surface(2, 2, 21, 0);
    let source_image = f.image(source);
    let back_image = f.image(f.back);
    let words = [0xff112233u32, 0x80445566, 0x50abcdef, 0x00123456];
    let locked = f.lock(source, 0, 0);
    assert_eq!(locked.Pitch, 8);
    f.m.cpu
        .write(locked.pBits, bytemuck::cast_slice(&words))
        .unwrap();
    f.okay("IDirect3DSurface9::UnlockRect", &[source]);
    assert_eq!(
        f.pixels(&source_image),
        bytemuck::cast_slice::<_, u8>(&words)
    );
    let from = f.packet(RECT {
        left: 0,
        top: 0,
        right: 2,
        bottom: 2,
    });
    let to = f.packet(RECT {
        left: 1,
        top: 1,
        right: 3,
        bottom: 3,
    });
    for inside in [false, true] {
        for filter in 0..=2 {
            f.okay(
                "IDirect3DDevice9::Clear",
                &[f.device, 0, 0, 1, 0xff102030, 1f32.to_bits(), 0],
            );
            if inside {
                f.okay("IDirect3DDevice9::BeginScene", &[f.device]);
            }
            f.okay(
                "IDirect3DDevice9::StretchRect",
                &[f.device, source, from, f.back, to, filter],
            );
            if inside {
                f.okay("IDirect3DDevice9::EndScene", &[f.device]);
            }
            let actual = f.pixels(&back_image);
            for y in 0..12 {
                for x in 0..16 {
                    let expected = if (1..3).contains(&x) && (1..3).contains(&y) {
                        words[(y - 1) * 2 + x - 1]
                    } else {
                        0xff102030
                    };
                    assert_eq!(
                        &actual[(y * 16 + x) * 4..(y * 16 + x) * 4 + 3],
                        &expected.to_le_bytes()[..3]
                    );
                }
            }
            assert_eq!(
                f.pixels(&source_image),
                bytemuck::cast_slice::<_, u8>(&words)
            );
        }
    }
    // A subrectangle writes through one staging allocation; readonly unlocks
    // never upload guest edits back to the image.
    let rect = f.packet(RECT {
        left: 1,
        top: 0,
        right: 2,
        bottom: 1,
    });
    let locked = f.lock(source, rect, 0);
    assert_eq!(locked.Pitch, 8);
    f.m.cpu.write_u32(locked.pBits, 0x408899aa).unwrap();
    f.okay("IDirect3DSurface9::UnlockRect", &[source]);
    let after = f.pixels(&source_image);
    assert_eq!(&after[4..8], &0x408899aau32.to_le_bytes());
    let readonly = f.lock(source, rect, 0x10);
    let submitted = f.submissions();
    f.m.cpu.write_u32(readonly.pBits, 0xffffffff).unwrap();
    f.okay("IDirect3DSurface9::UnlockRect", &[source]);
    assert_eq!(f.submissions(), submitted);
    assert_eq!(f.pixels(&source_image), after);
    let readonly = f.lock(source, rect, 0x10);
    assert_eq!(
        f.m.cpu.read_u32(readonly.pBits).unwrap(),
        0x408899aa,
        "next lock refreshes CPU staging from GPU"
    );
    f.okay("IDirect3DSurface9::UnlockRect", &[source]);
    assert_eq!(release(&mut env(&mut f.m), source), 0);
}

#[test]
fn plain_surface_identity_reset_and_rejected_blits_do_not_submit_or_leak() {
    let mut f = Fixture::new();
    let source = f.surface(2, 2, 21, 0);
    let output = f.packet(0u32);
    let iid = f.packet(resources::IID_SURFACE);
    f.okay("IDirect3DSurface9::QueryInterface", &[source, iid, output]);
    assert_eq!(f.m.cpu.read_u32(output).unwrap(), source);
    assert_eq!(release(&mut env(&mut f.m), source), 1);
    let desc_out = f.packet(D3DSURFACE_DESC::default());
    f.okay("IDirect3DSurface9::GetDesc", &[source, desc_out]);
    let desc: D3DSURFACE_DESC = GuestPtr::new(desc_out).read(&f.m.cpu).unwrap();
    assert_eq!(
        (desc.Format, desc.Pool, desc.Usage, desc.Width, desc.Height),
        (21, 0, 0, 2, 2)
    );
    let from = f.packet(RECT {
        left: 0,
        top: 0,
        right: 2,
        bottom: 2,
    });
    let scaled = f.packet(RECT {
        left: 0,
        top: 0,
        right: 3,
        bottom: 2,
    });
    let invalid = f.packet(RECT {
        left: -1,
        top: 0,
        right: 1,
        bottom: 2,
    });
    let readonly = f.m.sys.vm.alloc(&mut f.m.cpu, 4096, Prot::READ).unwrap();
    for (src, sr, dst, dr, filter, error) in [
        (source, from, f.back, scaled, 1, UNAVAILABLE),
        (source, 0, source, 0, 0, UNAVAILABLE),
        (source, invalid, f.back, from, 0, INVALID),
        (source, 0, f.back, from, 3, INVALID),
        (0, 0, f.back, from, 0, INVALID),
    ] {
        let submitted = f.submissions();
        let result = f.call(
            "IDirect3DDevice9::StretchRect",
            &[f.device, src, sr, dst, dr, filter],
        );
        assert_eq!(f.finish(result), error);
        assert_eq!(f.submissions(), submitted);
    }
    let result = f.call("IDirect3DSurface9::LockRect", &[source, readonly, 0, 0]);
    assert_eq!(f.finish(result), INVALID);
    assert!(
        resources::resource(&env(&mut f.m), source)
            .unwrap()
            .lock
            .is_none()
    );
    let locked = f.lock(source, 0, 0);
    let result = f.call(
        "IDirect3DDevice9::StretchRect",
        &[f.device, source, 0, f.back, from, 0],
    );
    assert_eq!(f.finish(result), INVALID);
    f.m.cpu.write_u32(locked.pBits, 0xff010203).unwrap();
    f.okay("IDirect3DSurface9::UnlockRect", &[source]);
    let parameters = f.packet(parameters());
    let outcome = f.call("IDirect3DDevice9::Reset", &[f.device, parameters]);
    assert_eq!(f.finish(outcome), INVALID);
    assert_eq!(release(&mut env(&mut f.m), source), 0);
    f.okay("IDirect3DDevice9::Reset", &[f.device, parameters]);
    assert_eq!(f.m.ext.get::<Surfaces>().targets.len(), 1);
    assert!(
        f.m.ext
            .get::<ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(api, _)| api.ends_with("IDirect3DDevice9::StretchRect"))
    );
    assert!(
        f.m.ext
            .get::<ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(_, reason)| {
                reason.contains("source=2x2 format=21 rect=(0,0,2,2)")
                    && reason.contains("destination=16x12 format=22 rect=(0,0,3,2); filter=1")
            })
    );
    assert_eq!(release(&mut env(&mut f.m), f.device), 0);
    assert_eq!(release(&mut env(&mut f.m), f.factory), 0);
    assert!(f.m.ext.get::<Surfaces>().targets.is_empty());
}

#[test]
fn recorded_blit_retains_image_leases_until_success_or_cancel() {
    let mut f = Fixture::new();
    for cancel in [false, true] {
        let src = f.surface(4, 4, 21, 0);
        let dst = f.render_target(2, 2, 21);
        let src_image = f.image(src);
        let dst_image = f.image(dst);
        let color = [0x12, 0x34, 0x56, 0x78].repeat(16);
        f.m.ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .upload(
                &src_image,
                Rect {
                    x: 0,
                    y: 0,
                    w: 4,
                    h: 4,
                },
                16,
                &color,
            )
            .unwrap();
        super::super::tests::force_recording_limit(&mut env(&mut f.m), f.device);
        let pause =
            f.m.ext
                .get::<State>()
                .gpu
                .as_ref()
                .unwrap()
                .context()
                .pause_submissions()
                .unwrap();
        let outcome = f.call(
            "IDirect3DDevice9::StretchRect",
            &[f.device, src, 0, dst, 0, 2],
        );
        assert!(matches!(outcome, Outcome::Block { .. }));
        assert_eq!(release(&mut env(&mut f.m), src), 0);
        assert_eq!(release(&mut env(&mut f.m), dst), 0);
        assert_eq!(
            f.m.ext.get::<Surfaces>().targets.len(),
            1,
            "recorded image leases outlive retired COM surfaces"
        );
        if cancel {
            super::super::cancel_pending(&mut f.m);
            drop(outcome);
            drop(pause);
        } else {
            drop(pause);
            assert_eq!(f.finish(outcome), 0);
            assert_eq!(f.pixels(&dst_image), [0x12, 0x34, 0x56, 0x78].repeat(4));
        }
        assert_eq!(f.m.ext.get::<Surfaces>().targets.len(), 1);
        assert!(resources::resource(&env(&mut f.m), src).is_none());
        assert!(resources::resource(&env(&mut f.m), dst).is_none());
        assert!(f.m.ext.get::<State>().pending.is_empty());
    }
}

#[test]
fn linear_full_source_scaling_matches_native_rgba_and_ignores_guest_draw_state() {
    // tools/fixtures/d3d9-stretch-native.c: HAL full-source 4 -> 3 LINEAR.
    // Native and WGPU may round UNORM8 interpolation by one channel unit.
    let native = [
        0x180b1111u32,
        0x183a114b,
        0x186911d8,
        0x603a4b11,
        0x60694b4b,
        0x60974bd8,
        0xdd69d811,
        0xdd97d84b,
        0xddc6d8d8,
    ];
    let native_enlarge = [
        0x10000a0au32,
        0x100e0a1a,
        0x10230a32,
        0x10380a50,
        0x104d0a80,
        0x10620ad4,
        0x230e1a0a,
        0x231c1a1a,
        0x23311a32,
        0x23461a50,
        0x235b1a80,
        0x23701ad4,
        0x4023320a,
        0x4031321a,
        0x40463232,
        0x405b3250,
        0x40703280,
        0x408532d4,
        0x6638500a,
        0x6646501a,
        0x665b5032,
        0x66705050,
        0x66855080,
        0x669a50d4,
        0x964d800a,
        0x965b801a,
        0x96708032,
        0x96858050,
        0x969a8080,
        0x96af80d4,
        0xd962d40a,
        0xd970d41a,
        0xd985d432,
        0xd99ad450,
        0xd9afd480,
        0xd9c4d4d4,
    ];
    let levels = [10u32, 50, 100, 240];
    let alpha = [16u32, 64, 128, 240];
    let input: [u32; 16] = std::array::from_fn(|i| {
        let (x, y) = (i % 4, i / 4);
        alpha[y] << 24 | ((x + y) as u32 * 35) << 16 | levels[y] << 8 | levels[x]
    });
    let mut f = Fixture::new();
    let source = f.surface(4, 4, 21, 0);
    let destination = f.render_target(8, 8, 21);
    let source_image = f.image(source);
    let destination_image = f.image(destination);
    let destination_target = f.target(destination);
    let locked = f.lock(source, 0, 0);
    f.m.cpu
        .write(locked.pBits, bytemuck::cast_slice(&input))
        .unwrap();
    f.okay("IDirect3DSurface9::UnlockRect", &[source]);
    // These states would reject or alter a guest primitive. A blit has its own
    // neutral copy state and leaves the guest's selected state unchanged.
    for (state, value) in [(7, 1), (15, 1), (25, 1), (27, 1), (168, 0)] {
        f.okay(
            "IDirect3DDevice9::SetRenderState",
            &[f.device, state, value],
        );
    }
    let full = f.packet(RECT {
        left: 0,
        top: 0,
        right: 4,
        bottom: 4,
    });
    for (left, top, width, expected) in [(2, 1, 3, &native[..]), (1, 1, 6, &native_enlarge[..])] {
        let to = f.packet(RECT {
            left,
            top,
            right: left + width,
            bottom: top + width,
        });
        for source_rect in [0, full] {
            f.m.ext
                .get_mut::<State>()
                .gpu
                .as_mut()
                .unwrap()
                .clear(
                    &destination_target,
                    Some([1. / 255., 2. / 255., 3. / 255., 170. / 255.]),
                    None,
                )
                .unwrap();
            f.okay("IDirect3DDevice9::BeginScene", &[f.device]);
            f.okay(
                "IDirect3DDevice9::StretchRect",
                &[f.device, source, source_rect, destination, to, 2],
            );
            f.okay("IDirect3DDevice9::EndScene", &[f.device]);
            let pixels = f.pixels(&destination_image);
            for y in 0..8 {
                for x in 0..8 {
                    let inside = (left as usize..(left + width) as usize).contains(&x)
                        && (top as usize..(top + width) as usize).contains(&y);
                    let value = if inside {
                        expected[(y - top as usize) * width as usize + x - left as usize]
                    } else {
                        0xaa010203
                    };
                    for (actual, expected) in pixels[(y * 8 + x) * 4..(y * 8 + x + 1) * 4]
                        .iter()
                        .zip(value.to_le_bytes())
                    {
                        let tolerance = if inside { 1 } else { 0 };
                        assert!(
                            (*actual as i16 - expected as i16).abs() <= tolerance,
                            "{width}x{width} pixel ({x},{y}) {actual} != {expected}"
                        );
                    }
                }
            }
            assert_eq!(
                f.pixels(&source_image),
                bytemuck::cast_slice::<_, u8>(&input)
            );
        }
    }
    // The actual 21 -> backbuffer 22 profile, with one unchanged axis and a
    // translated destination rectangle. X bytes do not define destination alpha.
    let one_axis = f.packet(RECT {
        left: 1,
        top: 1,
        right: 5,
        bottom: 4,
    });
    f.okay(
        "IDirect3DDevice9::StretchRect",
        &[f.device, source, full, f.back, one_axis, 2],
    );
    let back = f.image(f.back);
    let pixels = f.pixels(&back);
    for y in 0..3 {
        for x in 0..4 {
            let position = (y as f64 + 0.5) * 4. / 3. - 0.5;
            let low = position.floor() as usize;
            let high = (low + 1).min(3);
            let blend = position - low as f64;
            for channel in 0..3 {
                let a = input[low * 4 + x].to_le_bytes()[channel] as f64;
                let b = input[high * 4 + x].to_le_bytes()[channel] as f64;
                let expected = (a * (1. - blend) + b * blend).round() as i16;
                assert!(
                    (pixels[((y + 1) * 16 + x + 1) * 4 + channel] as i16 - expected).abs() <= 1
                );
            }
        }
    }
    let out = f.packet(0u32);
    f.okay("IDirect3DDevice9::GetRenderState", &[f.device, 168, out]);
    assert_eq!(f.m.cpu.read_u32(out).unwrap(), 0);
    assert_eq!(release(&mut env(&mut f.m), source), 0);
    assert_eq!(release(&mut env(&mut f.m), destination), 0);
}

#[test]
fn linear_scaling_handles_single_texel_axes_and_rejects_unsupported_profiles() {
    let mut f = Fixture::new();
    // Axis positions independently inferred from the 4 -> 6 and 4 -> 3 native
    // color ramps above. Combine them to exercise mixed scaling without a
    // second renderer or an extra native case matrix.
    let enlarge = [0., 0.4, 1., 1.6, 2.2, 2.8];
    let shrink = [1. / 6., 1.5, 17. / 6.];
    let center = [0.; 6];
    let destination = f.render_target(8, 8, 21);
    let destination_target = f.target(destination);
    for (w, h, xs, ys) in [
        (4, 4, &enlarge[..], &shrink[..]),
        (1, 4, &center[..], &shrink[..]),
        (4, 1, &shrink[..], &center[..]),
        (1, 1, &center[..5], &center[..5]),
    ] {
        let source = f.surface(w, h, 21, 0);
        let input: Vec<u32> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                (24 + y * 64) << 24 | (40 + x * 50) << 16 | (12 + y * 60) << 8 | ((x + y) * 30)
            })
            .collect();
        let locked = f.lock(source, 0, 0);
        assert_eq!(locked.Pitch, w as i32 * 4);
        f.m.cpu
            .write(locked.pBits, bytemuck::cast_slice(&input))
            .unwrap();
        f.okay("IDirect3DSurface9::UnlockRect", &[source]);
        f.m.ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .clear(
                &destination_target,
                Some([1. / 255., 2. / 255., 3. / 255., 170. / 255.]),
                None,
            )
            .unwrap();
        let to = f.packet(RECT {
            left: 1,
            top: 2,
            right: 1 + xs.len() as i32,
            bottom: 2 + ys.len() as i32,
        });
        f.okay(
            "IDirect3DDevice9::StretchRect",
            &[f.device, source, 0, destination, to, 2],
        );
        let pixels = f.pixels(&destination_target.image);
        for y in 0..8 {
            for x in 0..8 {
                if x < 1 || x > xs.len() || y < 2 || y >= 2 + ys.len() {
                    assert_eq!(
                        &pixels[(y * 8 + x) * 4..(y * 8 + x + 1) * 4],
                        &0xaa010203u32.to_le_bytes()
                    );
                    continue;
                }
                let (u, v) = (xs[x - 1], ys[y - 2]);
                let (x0, y0) = (u as usize, v as usize);
                let (x1, y1) = ((x0 + 1).min(w as usize - 1), (y0 + 1).min(h as usize - 1));
                let (a, b) = (u - x0 as f64, v - y0 as f64);
                for channel in 0..4 {
                    let pixel = |x: usize, y: usize| {
                        input[y * w as usize + x].to_le_bytes()[channel] as f64
                    };
                    let first = pixel(x0, y0) * (1. - a) + pixel(x1, y0) * a;
                    let second = pixel(x0, y1) * (1. - a) + pixel(x1, y1) * a;
                    let expected = (first * (1. - b) + second * b).round() as i16;
                    let actual = pixels[(y * 8 + x) * 4 + channel] as i16;
                    assert!(
                        (actual - expected).abs() <= 1,
                        "{w}x{h} at ({x},{y}) channel {channel}: {actual} != {expected}"
                    );
                }
            }
        }
        assert_eq!(release(&mut env(&mut f.m), source), 0);
    }
    let source = f.surface(4, 4, 21, 0);
    let plain = f.surface(3, 3, 21, 0);
    let cropped = f.packet(RECT {
        left: 1,
        top: 1,
        right: 3,
        bottom: 3,
    });
    let to = f.packet(RECT {
        left: 1,
        top: 1,
        right: 4,
        bottom: 4,
    });
    let submitted = f.submissions();
    for (src_rect, dst, dst_rect, filter) in [
        (0, destination, to, 0),
        (0, destination, to, 1),
        (cropped, destination, to, 2),
        (0, plain, 0, 2),
    ] {
        let result = f.call(
            "IDirect3DDevice9::StretchRect",
            &[f.device, source, src_rect, dst, dst_rect, filter],
        );
        assert_eq!(f.finish(result), UNAVAILABLE);
        assert_eq!(f.submissions(), submitted);
    }
    assert!(
        f.m.ext
            .get::<ApiDiagnostics>()
            .warnings
            .keys()
            .any(|(_, reason)| {
                reason.contains("render-target destination")
                    && reason.contains("source=4x4 format=21")
                    && reason.contains("destination=3x3 format=21 rect=(0,0,3,3); filter=2")
            })
    );
    // This guard is specific to scaling: the previously supported plain-to-plain
    // equal-size copy still succeeds, including an explicit partial source.
    let equal = f.packet(RECT {
        left: 0,
        top: 0,
        right: 3,
        bottom: 3,
    });
    f.okay(
        "IDirect3DDevice9::StretchRect",
        &[f.device, source, equal, plain, 0, 1],
    );
    assert_eq!(release(&mut env(&mut f.m), source), 0);
    assert_eq!(release(&mut env(&mut f.m), plain), 0);
    assert_eq!(release(&mut env(&mut f.m), destination), 0);
}

// spec: GFX-CAP-1
#[test]
fn color_fill_writes_default_pool_rectangles_and_rejects_other_pools() {
    let mut f = Fixture::new();
    let surface = f.surface(4, 2, 22, 0);
    let image = f.image(surface);
    f.okay(
        "IDirect3DDevice9::ColorFill",
        &[f.device, surface, 0, 0x00112233],
    );
    let rect = f.packet(RECT {
        left: 1,
        top: 1,
        right: 3,
        bottom: 2,
    });
    f.okay(
        "IDirect3DDevice9::ColorFill",
        &[f.device, surface, rect, 0x80445566],
    );
    let actual = f.pixels(&image);
    for y in 0..2 {
        for x in 0..4 {
            let expected = if y == 1 && (1..3).contains(&x) {
                0xff445566u32
            } else {
                0xff112233
            };
            let at = (y * 4 + x) * 4;
            assert_eq!(&actual[at..at + 4], &expected.to_le_bytes(), "({x}, {y})");
        }
    }
    let system = f.surface(4, 2, 21, 2);
    let outcome = f.call("IDirect3DDevice9::ColorFill", &[f.device, system, 0, 0]);
    assert_eq!(f.finish(outcome), INVALID);
    let outside = f.packet(RECT {
        left: 0,
        top: 0,
        right: 5,
        bottom: 2,
    });
    let outcome = f.call(
        "IDirect3DDevice9::ColorFill",
        &[f.device, surface, outside, 0],
    );
    assert_ne!(f.finish(outcome), 0);
    for handle in [surface, system] {
        assert_eq!(release(&mut env(&mut f.m), handle), 0);
    }
}

// spec: GFX-COPY-3
#[test]
fn d3d9_package_d_update_surface_full_and_rect_pixels() {
    let mut f = Fixture::new();
    f.m.ext
        .get_mut::<State>()
        .tables
        .insert("IDirect3DTexture9", 0);
    for levels in [false, true] {
        let output = f.packet(0u32);
        let mut surfaces = Vec::new();
        let mut textures = Vec::new();
        for pool in [2, 0] {
            let surface = if levels {
                f.okay(
                    "IDirect3DDevice9::CreateTexture",
                    &[
                        f.device,
                        8,
                        6,
                        2,
                        if pool == 0 { 0x200 } else { 0 },
                        21,
                        pool,
                        output,
                        0,
                    ],
                );
                let texture = f.m.cpu.read_u32(output).unwrap();
                textures.push(texture);
                f.okay("IDirect3DTexture9::GetSurfaceLevel", &[texture, 1, output]);
                f.m.cpu.read_u32(output).unwrap()
            } else {
                f.surface(4, 3, 21, pool)
            };
            surfaces.push(surface);
        }
        let (source, destination) = (surfaces[0], surfaces[1]);
        let input: Vec<u32> = (0..12).map(|n| 0xff102030 + n * 0x010203).collect();
        let locked = f.lock(source, 0, 0);
        f.m.cpu
            .write(locked.pBits, bytemuck::cast_slice(&input))
            .unwrap();
        f.okay("IDirect3DSurface9::UnlockRect", &[source]);
        f.okay(
            "IDirect3DDevice9::UpdateSurface",
            &[f.device, source, 0, destination, 0],
        );
        if levels {
            f.okay("IDirect3DDevice9::SetTexture", &[f.device, 0, textures[1]]);
            let locked = f.lock(destination, 0, 0x10);
            assert_eq!(f.m.cpu.read_u32(locked.pBits).unwrap(), input[0]);
            f.okay("IDirect3DSurface9::UnlockRect", &[destination]);
        }
        let image = f.image(destination);
        assert_eq!(f.pixels(&image), bytemuck::cast_slice::<_, u8>(&input));
        let rect = f.packet(RECT {
            left: 1,
            top: 1,
            right: 3,
            bottom: 3,
        });
        let point = f.packet(POINT { x: 0, y: 0 });
        f.okay(
            "IDirect3DDevice9::UpdateSurface",
            &[f.device, source, rect, destination, point],
        );
        let mut expected = input.clone();
        for y in 0..2 {
            for x in 0..2 {
                expected[y * 4 + x] = input[(y + 1) * 4 + x + 1];
            }
        }
        assert_eq!(f.pixels(&image), bytemuck::cast_slice::<_, u8>(&expected));
    }
}

// spec: GFX-COPY-3
#[test]
fn d3d9_package_d_update_surface_rejects_pools_formats_locks_and_bounds() {
    let mut f = Fixture::new();
    let system = f.surface(4, 3, 21, 2);
    let default = f.surface(4, 3, 21, 0);
    let other_format = f.surface(4, 3, 22, 0);
    for (source, destination) in [(default, default), (system, system), (system, other_format)] {
        let outcome = f.call(
            "IDirect3DDevice9::UpdateSurface",
            &[f.device, source, 0, destination, 0],
        );
        assert_eq!(f.finish(outcome), INVALID);
    }
    for surface in [system, default] {
        f.lock(surface, 0, 0);
        let outcome = f.call(
            "IDirect3DDevice9::UpdateSurface",
            &[f.device, system, 0, default, 0],
        );
        assert_eq!(f.finish(outcome), INVALID);
        f.okay("IDirect3DSurface9::UnlockRect", &[surface]);
    }
    let rect = f.packet(RECT {
        left: 0,
        top: 0,
        right: 5,
        bottom: 3,
    });
    let point = f.packet(POINT { x: 1, y: 0 });
    for (rect, point) in [(rect, 0), (0, point)] {
        let outcome = f.call(
            "IDirect3DDevice9::UpdateSurface",
            &[f.device, system, rect, default, point],
        );
        assert_eq!(f.finish(outcome), INVALID);
    }
}
