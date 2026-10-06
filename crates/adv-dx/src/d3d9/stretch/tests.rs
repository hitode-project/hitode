//! Authored 1:1 color blits over one shared GPU and one readback worker.
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
struct Fixture {
    m: Machine<JitCpu>,
    device: u32,
    factory: u32,
    back: u32,
    wake: Arc<adv_sync::WakeEvent>,
    _service: adv_sync::background::Service,
}
impl Fixture {
    fn new() -> Self {
        let mut m = super::super::tests::machine();
        // The ownership-only base deliberately limits references to three.
        // This fixture exercises two live DEFAULT resources and a device wait.
        m.sys.objects = Objects::default();
        m.ext.get_mut::<State>().com = Com::new(&mut m.sys.objects).unwrap();
        m.cpu.map(STACK, 4096, Prot::RW).unwrap();
        m.cpu.map(0x400000, 4096, Prot::RX).unwrap();
        m.spawn("authored color blits", 0x400000, &[], 65536)
            .unwrap();
        let gpu = pollster::block_on(Gpu::request(true)).unwrap();
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
    fn packet<T: bytemuck::Pod>(&mut self, value: T) -> u32 {
        let mut e = env(&mut self.m);
        let address = alloc(&mut e, std::mem::size_of::<T>() as u32).unwrap();
        GuestPtr::new(address)
            .write(&mut self.m.cpu, value)
            .unwrap();
        address
    }
    fn call(&mut self, name: &str, args: &[u32]) -> Outcome {
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
    fn finish(&mut self, outcome: Outcome) -> u32 {
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
        let Outcome::Return(result) = outcome else {
            panic!("expected command completion")
        };
        result
    }
    fn okay(&mut self, name: &str, args: &[u32]) {
        let outcome = self.call(name, args);
        assert_eq!(self.finish(outcome), 0, "{name}");
    }
    fn surface(&mut self, width: u32, height: u32, format: u32, pool: u32) -> u32 {
        let output = self.packet(0u32);
        self.okay(
            "IDirect3DDevice9::CreateOffscreenPlainSurface",
            &[self.device, width, height, format, pool, output, 0],
        );
        self.m.cpu.read_u32(output).unwrap()
    }
    fn lock(&mut self, surface: u32, rect: u32, flags: u32) -> D3DLOCKED_RECT {
        let output = self.packet(D3DLOCKED_RECT::default());
        self.okay(
            "IDirect3DSurface9::LockRect",
            &[surface, output, rect, flags],
        );
        GuestPtr::new(output).read(&self.m.cpu).unwrap()
    }
    fn image(&mut self, surface: u32) -> Image {
        let e = env(&mut self.m);
        let r = resources::resource(&e, surface).unwrap();
        (e.ext.get::<State>().provider.target)(e.ext, r.surface.unwrap())
            .unwrap()
            .image
    }
    fn pixels(&mut self, image: &Image) -> Vec<u8> {
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
    fn submissions(&self) -> u64 {
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
        (source, from, f.back, scaled, 2, UNAVAILABLE),
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
    assert_eq!(release(&mut env(&mut f.m), f.device), 0);
    assert_eq!(release(&mut env(&mut f.m), f.factory), 0);
    assert!(f.m.ext.get::<Surfaces>().targets.is_empty());
}

#[test]
fn pending_blit_retains_both_controlling_surfaces_until_success_or_cancel() {
    let mut f = Fixture::new();
    for cancel in [false, true] {
        let src = f.surface(2, 2, 21, 0);
        let dst = f.surface(2, 2, 21, 0);
        let src_image = f.image(src);
        let dst_image = f.image(dst);
        let color = vec![0x12, 0x34, 0x56, 0x78].repeat(4);
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
                    w: 2,
                    h: 2,
                },
                8,
                &color,
            )
            .unwrap();
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
            &[f.device, src, 0, dst, 0, 0],
        );
        assert!(matches!(outcome, Outcome::Block { .. }));
        assert_eq!(release(&mut env(&mut f.m), src), 0);
        assert_eq!(release(&mut env(&mut f.m), dst), 0);
        assert_eq!(
            f.m.ext.get::<Surfaces>().targets.len(),
            3,
            "pending internal refs own both images"
        );
        if cancel {
            super::super::cancel_pending(&mut f.m);
            drop(outcome);
            drop(pause);
        } else {
            drop(pause);
            assert_eq!(f.finish(outcome), 0);
            assert_eq!(f.pixels(&dst_image), color);
        }
        assert_eq!(f.m.ext.get::<Surfaces>().targets.len(), 1);
        assert!(resources::resource(&env(&mut f.m), src).is_none());
        assert!(resources::resource(&env(&mut f.m), dst).is_none());
        assert!(f.m.ext.get::<State>().pending.is_empty());
    }
}
