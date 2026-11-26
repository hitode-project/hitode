use super::*;
use crate::ddraw::{IID_DDRAW, IID_DDRAW7, IID_SURFACE, IID_SURFACE4, creation, front};
use adv_abi::{Env, Outcome, Registry, com::IUNKNOWN, gfx_iids::*};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, Reg},
};
use adv_device::surface::{MemoryOperations, MemoryView, Snapshot, SurfaceStore};
use adv_machine::{Clock, Machine, layout::PARK_ADDR};
use std::borrow::Cow;

const DATA: u32 = 0x410000;
const STACK: u32 = DATA + 0xc00;
const OUT: u32 = DATA + 0x20;

struct Fixture {
    machine: Machine<JitCpu>,
}

fn memory(cpu: &mut dyn Guest) -> MemoryView<'_, dyn Guest + '_> {
    MemoryView::new(
        cpu,
        MemoryOperations {
            read: |cpu, address, len| {
                let mut bytes = vec![0; len];
                cpu.read(address, &mut bytes).map_err(|e| e.to_string())?;
                Ok(Cow::Owned(bytes))
            },
            write: |cpu, address, bytes| cpu.write(address, bytes).map_err(|e| e.to_string()),
            writable: |cpu, address, len| {
                adv_abi::guest::check_range(cpu, address, len as u32, Prot::WRITE)
                    .map_err(|e| e.to_string())
            },
            copy: |cpu, src, dst, len| cpu.copy_memory(src, dst, len).map_err(|e| e.to_string()),
        },
    )
}

impl Fixture {
    fn new(d3d: bool) -> Self {
        let mut registry = Registry::default();
        front::register(&mut registry);
        if d3d {
            register(&mut registry);
        }
        assert!(registry.signature_conflicts().is_empty());
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, false).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        machine.cpu.map(0x500000, 0x400000, Prot::RW).unwrap();
        machine
            .ext
            .insert(SurfaceStore::new(32, 32, 0x500000, 0x400000, Snapshot::default()).unwrap());
        front::install(
            &mut machine,
            front::Provider {
                dc: crate::interop::MemoryDc {
                    create: |_, _, _| unreachable!(),
                    flush: |_, _, _, _| unreachable!(),
                    release: |_, _, _| unreachable!(),
                    discard: |_, _| unreachable!(),
                },
                memory,
                window: |_, _, _| None,
                submitted: |_, _, _| {},
                completed: |_, _| None,
                released: |_, _| {},
                vsync: |_| None,
            },
        )
        .unwrap();
        if d3d {
            install(&mut machine).unwrap();
        }
        Self { machine }
    }

    fn write(&mut self, address: u32, bytes: &[u8]) {
        self.machine.cpu.write(address, bytes).unwrap();
    }

    fn word(&self, address: u32) -> u32 {
        self.machine.cpu.read_u32(address).unwrap()
    }

    fn call(&mut self, name: &str, args: &[u32]) -> u32 {
        self.invoke(name, args, |_, _| panic!("{name}: unexpected callback"))
    }

    fn invoke(
        &mut self,
        name: &str,
        args: &[u32],
        mut callback: impl FnMut(&dyn Guest, &[u32]) -> u32,
    ) -> u32 {
        let m = &mut self.machine;
        m.sys.thunks.publish(&mut m.cpu, "ddraw.dll", name).unwrap();
        let thunk = m.sys.thunks.published_index("ddraw.dll", name).unwrap();
        let f = m.sys.thunks.get(thunk).f;
        assert_eq!(usize::from(f.conv.argc()), args.len(), "{name}");
        m.cpu.set_reg(Reg::Esp, STACK);
        for (i, &arg) in args.iter().enumerate() {
            m.cpu.write_u32(STACK + 4 + i as u32 * 4, arg).unwrap();
        }
        let mut env = Env {
            context: m.sys.current,
            cpu: &mut m.cpu,
            sys: &mut m.sys,
            ext: &mut m.ext,
            thunk,
        };
        let mut outcome = (f.handler)(&mut env);
        loop {
            match outcome {
                Outcome::Return(hr) => break hr,
                Outcome::Call { args, then, .. } => {
                    let returned = callback(env.cpu, &args);
                    outcome = then(&mut env, adv_machine::Resume::Returned(returned));
                }
                Outcome::Fail(error) => panic!("{name}: {error}"),
                Outcome::Return64(_)
                | Outcome::ReturnF32(_)
                | Outcome::ReturnF64(_)
                | Outcome::ResumeExecution(_)
                | Outcome::Raise(_)
                | Outcome::ReturnFromCall(_)
                | Outcome::Block { .. }
                | Outcome::Defer { .. }
                | Outcome::HostRequest { .. }
                | Outcome::ExitThread(_)
                | Outcome::ExitProcess(_) => panic!("{name}: unexpected continuation"),
            }
        }
    }

    fn dd(&mut self, version: u32) -> u32 {
        self.write(DATA, &IID_DDRAW7);
        let hr = if version == 7 {
            self.call("DirectDrawCreateEx", &[0, OUT, DATA, 0])
        } else {
            self.call("DirectDrawCreate", &[0, OUT, 0])
        };
        assert_eq!(hr, 0);
        let dd = self.word(OUT);
        assert_eq!(
            self.call("IDirectDraw7::SetCooperativeLevel", &[dd, 0, 8]),
            0
        );
        dd
    }

    fn query(&mut self, this: u32, iid: [u8; 16]) -> (u32, u32) {
        self.write(DATA, &iid);
        let hr = self.call("IDirectDraw7::QueryInterface", &[this, DATA, OUT]);
        (hr, self.word(OUT))
    }

    fn face(&mut self, this: u32, iid: [u8; 16]) -> u32 {
        let (hr, pointer) = self.query(this, iid);
        assert_eq!(hr, 0);
        assert_ne!(pointer, 0);
        pointer
    }

    fn qi(&mut self, interface: &str, this: u32, iid: [u8; 16]) -> (u32, u32) {
        self.write(DATA, &iid);
        let hr = self.call(&format!("{interface}::QueryInterface"), &[this, DATA, OUT]);
        (hr, self.word(OUT))
    }

    fn surface(&mut self, dd: u32, modern: bool, caps: u32, width: u32, height: u32) -> u32 {
        let desc = creation::Described {
            flags: creation::DDSD_CAPS | creation::DDSD_WIDTH | creation::DDSD_HEIGHT,
            caps: [caps, 0, 0, 0],
            width,
            height,
            ..Default::default()
        };
        self.write(
            DATA + 0x100,
            &desc.bytes(if modern { 124 } else { 108 }, 0, 0),
        );
        assert_eq!(
            self.call("IDirectDraw7::CreateSurface", &[dd, DATA + 0x100, OUT, 0]),
            0
        );
        self.word(OUT)
    }

    fn output(&mut self, method: &str, args: &[u32]) -> u32 {
        assert_eq!(self.call(method, args), 0, "{method}");
        self.word(OUT)
    }

    fn device_call(&mut self, device: u32, name: &str, args: &[u32]) -> u32 {
        let args = [device]
            .into_iter()
            .chain(args.iter().copied())
            .collect::<Vec<_>>();
        self.call(&format!("IDirect3DDevice7::{name}"), &args)
    }

    fn lock(&mut self, surface: u32, flags: u32) -> (u32, u32) {
        self.write(DATA + 0x100, &124u32.to_le_bytes());
        assert_eq!(
            self.call(
                "IDirectDrawSurface7::Lock",
                &[surface, 0, DATA + 0x100, flags, 0]
            ),
            0
        );
        (self.word(DATA + 0x100 + 36), self.word(DATA + 0x100 + 16))
    }

    fn unlock(&mut self, surface: u32) {
        assert_eq!(self.call("IDirectDrawSurface7::Unlock", &[surface, 0]), 0);
    }

    fn pixels(&mut self, surface: u32, width: u32, height: u32) -> Vec<u32> {
        let (address, pitch) = self.lock(surface, 0x10);
        let mut pixels = Vec::new();
        for y in 0..height {
            for x in 0..width {
                pixels.push(self.word(address + y * pitch + x * 4) & 0xffffff);
            }
        }
        self.unlock(surface);
        if let Some(d3d) = &self.machine.ext.get::<front::State>().draw.d3d {
            d3d.gpu.check().unwrap();
        }
        pixels
    }

    fn device(&mut self, d3d: u32, face: Face, surface: u32) -> u32 {
        self.device_kind(d3d, face, surface, caps::Device::Hal)
    }

    fn device_kind(&mut self, d3d: u32, face: Face, surface: u32, kind: caps::Device) -> u32 {
        self.write(DATA, &kind.guid());
        let mut args = vec![d3d, DATA, surface, OUT];
        if face == Face::Direct3D3 {
            args.push(0);
        }
        self.output(&format!("{}::CreateDevice", face.name()), &args)
    }

    fn described_surface(&mut self, dd: u32, desc: creation::Described) -> u32 {
        self.write(DATA + 0x100, &desc.bytes(124, 0, 0));
        self.output("IDirectDraw7::CreateSurface", &[dd, DATA + 0x100, OUT, 0])
    }
}

fn distinct(pointers: &[u32]) {
    for (i, &p) in pointers.iter().enumerate() {
        assert_ne!(p, 0);
        assert!(!pointers[..i].contains(&p), "duplicate pointer {p:#x}");
    }
}

// spec: GFX-D3D7-1
#[test]
fn directdraw_and_surface_faces_share_the_measured_counts() {
    let mut f = Fixture::new(true);
    let dd = f.dd(1);
    let old = f.face(dd, IID_IDIRECT3D);
    let d3d2 = f.face(dd, IID_IDIRECT3D2);
    let d3d3 = f.face(dd, IID_IDIRECT3D3);
    distinct(&[dd, old, d3d2, d3d3]);
    assert_eq!(f.query(dd, IID_IDIRECT3D7), (NO_INTERFACE, 0));
    assert_eq!(f.qi("IDirect3D", old, IID_IDIRECT3D2), (0, d3d2));
    assert_eq!(f.call("IDirect3D2::Release", &[d3d2]), 4);
    assert_eq!(f.qi("IDirect3D", old, IID_DDRAW), (0, dd));
    assert_eq!(f.call("IDirectDraw7::Release", &[dd]), 4);
    let dd7 = f.face(dd, IID_DDRAW7);
    assert_eq!(f.query(dd7, IID_IDIRECT3D7), (NO_INTERFACE, 0));
    assert_eq!(f.query(dd7, IID_IDIRECT3D3), (0, d3d3));
    let ex = f.dd(7);
    let d3d7 = f.face(ex, IID_IDIRECT3D7);
    assert_eq!(f.query(ex, IID_IDIRECT3D3), (NO_INTERFACE, 0));
    let rt7 = f.surface(
        ex,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        32,
        32,
    );
    let dev7 = f.device(d3d7, Face::Direct3D7, rt7);
    assert_eq!(
        f.qi("IDirect3DDevice7", dev7, IID_IDIRECT3DDEVICE3),
        (NO_INTERFACE, 0)
    );

    let target = f.surface(
        dd,
        false,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        32,
        32,
    );
    let target4 = f.face(target, IID_SURFACE4);
    let hal = f.face(target, IID_IDIRECT3DHALDEVICE);
    let rgb = f.face(target, IID_IDIRECT3DRGBDEVICE);
    distinct(&[target, hal, rgb]);
    assert_eq!(f.query(target, IID_IDIRECT3DHALDEVICE), (0, hal));
    assert_eq!(f.call("IDirect3DDevice::Release", &[hal]), 3);
    assert_eq!(f.call("IDirect3DDevice::Release", &[rgb]), 2);
    assert_eq!(f.qi("IDirect3DDevice", hal, IID_SURFACE), (0, target));
    assert_eq!(f.call("IDirectDrawSurface7::Release", &[target]), 2);
    assert_eq!(
        f.qi("IDirect3DDevice", hal, IID_IDIRECT3DDEVICE2),
        (INVALID, 0)
    );
    assert_eq!(
        f.qi("IDirect3DDevice", hal, IID_IDIRECT3DDEVICE3),
        (INVALID, 0)
    );
    let dev2 = f.device(d3d2, Face::Direct3D2, target);
    let dev3 = f.device(d3d3, Face::Direct3D3, target4);
    let dev2_again = f.device(d3d2, Face::Direct3D2, target);
    let (_, dev2_v1) = f.qi("IDirect3DDevice2", dev2, IID_IDIRECT3DDEVICE);
    let (_, dev3_v2) = f.qi("IDirect3DDevice3", dev3, IID_IDIRECT3DDEVICE2);
    let (_, dev3_v1) = f.qi("IDirect3DDevice3", dev3, IID_IDIRECT3DDEVICE);
    distinct(&[hal, dev2, dev3, dev2_again, dev2_v1, dev3_v2, dev3_v1]);
    assert_eq!(
        f.qi("IDirect3DDevice2", dev2, IID_IDIRECT3DDEVICE3),
        (NO_INTERFACE, 0)
    );
}

#[test]
fn children_hold_one_reference_and_device_handles_are_local() {
    let mut f = Fixture::new(true);
    let dd = f.dd(1);
    let d3d = f.face(dd, IID_IDIRECT3D);
    let d3d2 = f.face(dd, IID_IDIRECT3D2);
    let target = f.surface(
        dd,
        false,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        32,
        32,
    );
    let dev1 = f.face(target, IID_IDIRECT3DHALDEVICE);
    let dev2 = f.device(d3d2, Face::Direct3D2, target);
    let m1 = f.output("IDirect3D::CreateMaterial", &[d3d, OUT, 0]);
    let m1_other = f.output("IDirect3D::CreateMaterial", &[d3d, OUT, 0]);
    let (_, m2) = f.qi("IDirect3DMaterial", m1, IID_IDIRECT3DMATERIAL2);
    let (_, m3) = f.qi("IDirect3DMaterial", m1, IID_IDIRECT3DMATERIAL3);
    distinct(&[m1, m1_other, m2, m3]);
    let h1 = f.output("IDirect3DMaterial::GetHandle", &[m1, dev1, OUT]);
    assert_eq!(
        f.output("IDirect3DMaterial::GetHandle", &[m1, dev1, OUT]),
        h1
    );
    let h1_other = f.output("IDirect3DMaterial::GetHandle", &[m1_other, dev1, OUT]);
    let h2 = f.output("IDirect3DMaterial2::GetHandle", &[m2, dev2, OUT]);
    distinct(&[m1, h1, h1_other, h2]);
    assert_eq!(f.call("IDirect3DMaterial2::Release", &[m2]), 2);
    assert_eq!(f.call("IDirect3DMaterial3::Release", &[m3]), 1);
    let tex = f.surface(dd, false, creation::TEXTURE, 8, 8);
    let tex1 = f.face(tex, IID_IDIRECT3DTEXTURE);
    let tex2 = f.face(tex, IID_IDIRECT3DTEXTURE2);
    distinct(&[tex, tex1, tex2]);
    assert_eq!(f.qi("IDirect3DTexture", tex1, IID_SURFACE), (0, tex));
    assert_eq!(
        f.output("IDirect3DTexture::GetHandle", &[tex1, dev1, OUT]),
        0x14
    );
    assert_eq!(
        f.output("IDirect3DTexture2::GetHandle", &[tex2, dev2, OUT]),
        0x14
    );
    assert_eq!(f.call("IDirect3DTexture::Release", &[tex1]), 3);
    let view = f.output("IDirect3D::CreateViewport", &[d3d, OUT, 0]);
    assert_eq!(
        f.qi("IDirect3DViewport3", view, IID_IDIRECT3DVIEWPORT2),
        (0, view)
    );
    assert_eq!(f.call("IDirect3DViewport3::Release", &[view]), 1);
    assert_eq!(f.call("IDirect3DDevice::AddViewport", &[dev1, view]), 0);
    assert_eq!(
        f.call("IDirect3DDevice::AddViewport", &[dev1, view]),
        INVALID
    );
    assert_eq!(
        f.call("IDirect3DDevice2::AddViewport", &[dev2, view]),
        INVALID
    );
    let light = f.output("IDirect3D::CreateLight", &[d3d, OUT, 0]);
    assert_eq!(
        f.qi("IDirect3DLight", light, IID_IDIRECT3DLIGHT),
        (0, light)
    );
    assert_eq!(f.call("IDirect3DLight::Release", &[light]), 1);
    assert_eq!(f.call("IDirect3DViewport3::AddLight", &[view, light]), 0);
    assert_eq!(
        f.call("IDirect3DViewport3::AddLight", &[view, light]),
        0x887602ef
    );
    assert_eq!(f.call("IDirect3DLight::Release", &[light]), 1);
    assert_eq!(f.call("IDirect3DViewport3::Release", &[view]), 1);
    assert_eq!(f.call("IDirect3DDevice::DeleteViewport", &[dev1, view]), 0);
    assert_eq!(f.qi("IDirect3DLight", light, IUNKNOWN), (INVALID, 0));
}

// spec: GFX-D3D7-1
#[test]
fn production_directdraw_keeps_refusing_direct3d() {
    let forced = std::env::var_os("ADV_DDRAW_LEGACY").is_some_and(|value| value == "1");
    let mut f = Fixture::new(forced);
    let old = f.dd(1);
    let ex = f.dd(7);
    for dd in [old, ex] {
        for iid in [
            IID_IDIRECT3D,
            IID_IDIRECT3D2,
            IID_IDIRECT3D3,
            IID_IDIRECT3D7,
        ] {
            assert_eq!(f.query(dd, iid), (NO_INTERFACE, 0));
        }
    }
}

// spec: GFX-D3D7-3
#[test]
fn device7_state_blocks_capture_neutral_registers_and_matrices() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        32,
        32,
    );
    let dev = f.device(d3d, Face::Direct3D7, rt);
    let w = |f: &mut Fixture, name, args: &[u32]| {
        let args = [dev]
            .into_iter()
            .chain(args.iter().copied())
            .collect::<Vec<_>>();
        f.call(&format!("IDirect3DDevice7::{name}"), &args)
    };
    assert_eq!(w(&mut f, "GetRenderState", &[41, OUT]), 0);
    assert_eq!(f.word(OUT), 0);
    assert_eq!(w(&mut f, "BeginStateBlock", &[]), 0);
    assert_eq!(w(&mut f, "BeginStateBlock", &[]), 0x88760835);
    assert_eq!(w(&mut f, "SetRenderState", &[41, 1]), 0);
    assert_eq!(w(&mut f, "SetTextureStageState", &[1, 18, 3]), 0);
    let matrix = [2f32; 16];
    f.write(DATA + 0x100, bytemuck::bytes_of(&matrix));
    assert_eq!(w(&mut f, "SetTransform", &[16, DATA + 0x100]), 0);
    assert_eq!(w(&mut f, "GetRenderState", &[41, OUT]), 0);
    assert_eq!(f.word(OUT), 0);
    assert_eq!(w(&mut f, "EndStateBlock", &[OUT]), 0);
    let block = f.word(OUT);
    assert_eq!(w(&mut f, "ApplyStateBlock", &[block]), 0);
    assert_eq!(w(&mut f, "GetRenderState", &[41, OUT]), 0);
    assert_eq!(f.word(OUT), 1);
    assert_eq!(w(&mut f, "GetTextureStageState", &[1, 18, OUT]), 0);
    assert_eq!(f.word(OUT), 3);
    assert_eq!(w(&mut f, "GetTransform", &[16, OUT]), 0);
    assert_eq!(f.word(OUT), 2f32.to_bits());
    assert_eq!(w(&mut f, "SetRenderState", &[41, 0]), 0);
    assert_eq!(w(&mut f, "CaptureStateBlock", &[block]), 0);
    assert_eq!(w(&mut f, "SetRenderState", &[41, 1]), 0);
    assert_eq!(w(&mut f, "ApplyStateBlock", &[block]), 0);
    assert_eq!(w(&mut f, "GetRenderState", &[41, OUT]), 0);
    assert_eq!(f.word(OUT), 0);
    assert_eq!(w(&mut f, "DeleteStateBlock", &[block]), 0);
    assert_eq!(w(&mut f, "ApplyStateBlock", &[block]), 0x88760834);
}

const EXPECTATIONS: &str = include_str!("expectations.tsv");

fn pixel_row(label: &str, device: &str, case: &str, row: &str) -> Vec<u32> {
    let raw = EXPECTATIONS
        .lines()
        .find_map(|line| {
            let mut fields = line.split('\t');
            (fields.next()? == label
                && fields.next()? == device
                && fields.next()? == case
                && fields.next()? == row)
                .then(|| fields.next().unwrap())
        })
        .unwrap();
    let digits = if label == "packed" { 4 } else { 6 };
    (0..raw.len())
        .step_by(digits)
        .map(|i| u32::from_str_radix(&raw[i..i + digits], 16).unwrap())
        .collect()
}

fn quad(x: f32, y: f32, right: f32, bottom: f32, color: u32) -> [[u32; 8]; 4] {
    std::array::from_fn(|i| {
        let [x, y] = [
            if i & 1 == 0 { x } else { right },
            if i & 2 == 0 { y } else { bottom },
        ];
        [
            x.to_bits(),
            y.to_bits(),
            0.5f32.to_bits(),
            1f32.to_bits(),
            color,
            0,
            ((i & 1) as f32).to_bits(),
            ((i >> 1) as f32).to_bits(),
        ]
    })
}

fn draw_tl(f: &mut Fixture, device: u32, primitive: u32, vertices: &[[u32; 8]]) {
    f.write(DATA + 0x200, bytemuck::cast_slice(vertices));
    assert_eq!(f.device_call(device, "BeginScene", &[]), 0);
    assert_eq!(
        f.device_call(
            device,
            "DrawPrimitive",
            &[primitive, 0x1c4, DATA + 0x200, vertices.len() as u32, 0]
        ),
        0
    );
    assert_eq!(f.device_call(device, "EndScene", &[]), 0);
}

// spec: GFX-D3D7-3, GFX-D3D7-5
#[test]
fn device7_draws_transformed_vertices_into_a_lockable_surface() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        32,
        32,
    );
    let dev = f.device(d3d, Face::Direct3D7, rt);
    assert_eq!(f.device_call(dev, "EndScene", &[]), 0x887602f9);
    assert_eq!(f.device_call(dev, "SetRenderState", &[22, 1]), 0);
    for (case, p) in [
        ("quad-int-1-1-5-5", [1., 1., 5., 5.]),
        ("quad-half-1.5-1.5-5.5-5.5", [1.5, 1.5, 5.5, 5.5]),
        ("quad-0.49-0.49-4.49-4.49", [1.49, 1.49, 5.49, 5.49]),
        ("quad-0.51-0.51-4.51-4.51", [1.51, 1.51, 5.51, 5.51]),
        ("quad-thin-1-1-5-1.4", [1., 1., 5., 1.4]),
    ] {
        assert_eq!(
            f.device_call(dev, "Clear", &[0, 0, 1, 0, 1f32.to_bits(), 0]),
            0
        );
        draw_tl(&mut f, dev, 5, &quad(p[0], p[1], p[2], p[3], u32::MAX));
        let pixels = f.pixels(rt, 7, 7);
        let expected = (0..7)
            .flat_map(|y| pixel_row("px", "hal", case, &y.to_string()))
            .collect::<Vec<_>>();
        assert_eq!(pixels, expected, "{case}");
    }
}

// spec: GFX-D3D7-5, GFX-BLT-1
#[test]
fn blit_between_recorded_draws_updates_both_texture_memory_classes() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let caps = creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE | creation::VIDEO_MEMORY;
    let source = f.surface(dd, true, caps, 4, 4);
    let output = f.surface(dd, true, caps, 4, 4);
    let dev = f.device(d3d, Face::Direct3D7, source);
    assert_eq!(f.device_call(dev, "SetRenderState", &[22, 1]), 0);
    for (pool, color) in [
        (creation::VIDEO_MEMORY, 0xff804020),
        (creation::SYSTEM_MEMORY, 0xff20c080),
    ] {
        let tex = f.surface(dd, true, creation::TEXTURE | pool, 4, 4);
        assert_eq!(f.device_call(dev, "SetRenderTarget", &[source, 0]), 0);
        assert_eq!(f.device_call(dev, "SetTexture", &[0, 0]), 0);
        assert_eq!(f.device_call(dev, "BeginScene", &[]), 0);
        f.write(
            DATA + 0x200,
            bytemuck::cast_slice(&quad(0., 0., 4., 4., color)),
        );
        assert_eq!(
            f.device_call(dev, "DrawPrimitive", &[5, 0x1c4, DATA + 0x200, 4, 0]),
            0
        );
        assert_eq!(
            f.call("IDirectDrawSurface7::Blt", &[tex, 0, source, 0, 0, 0]),
            0
        );
        assert_eq!(f.device_call(dev, "SetRenderTarget", &[output, 0]), 0);
        assert_eq!(f.device_call(dev, "SetTexture", &[0, tex]), 0);
        f.write(
            DATA + 0x200,
            bytemuck::cast_slice(&quad(0., 0., 4., 4., u32::MAX)),
        );
        assert_eq!(
            f.device_call(dev, "DrawPrimitive", &[5, 0x1c4, DATA + 0x200, 4, 0]),
            0
        );
        assert_eq!(f.device_call(dev, "EndScene", &[]), 0);
        assert_eq!(f.pixels(output, 4, 4), [color & 0xffffff; 16]);
    }
}

// spec: GFX-D3D7-4, GFX-D3D7-5
#[test]
fn device7_color_key_tests_raw_bits_before_filtering() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        64,
        4,
    );
    let tex = f.surface(dd, true, creation::TEXTURE, 4, 1);
    let dev = f.device(d3d, Face::Direct3D7, rt);
    let (address, _) = f.lock(tex, 0);
    f.write(
        address,
        bytemuck::cast_slice(&[0xffff00ffu32, 0xffff00ff, u32::MAX, u32::MAX]),
    );
    f.unlock(tex);
    f.write(DATA + 0x180, bytemuck::bytes_of(&[0xff00ffu32; 2]));
    assert_eq!(
        f.call("IDirectDrawSurface7::SetColorKey", &[tex, 8, DATA + 0x180]),
        0
    );
    assert_eq!(f.device_call(dev, "SetRenderState", &[22, 1]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[19, 5]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[20, 6]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[24, 0x80]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[25, 7]), 0);
    assert_eq!(f.device_call(dev, "SetTexture", &[0, tex]), 0);
    assert_eq!(f.device_call(dev, "SetTextureStageState", &[0, 12, 3]), 0);
    for i in 0..6 {
        let mode = format!("mode{i}");
        let filter = if matches!(i, 0 | 4) { 1 } else { 2 };
        assert_eq!(
            f.device_call(dev, "SetRenderState", &[41, u32::from(i != 3)]),
            0
        );
        assert_eq!(
            f.device_call(dev, "SetRenderState", &[27, u32::from(matches!(i, 2 | 4))]),
            0
        );
        assert_eq!(
            f.device_call(dev, "SetRenderState", &[15, u32::from(i == 5)]),
            0
        );
        assert_eq!(
            f.device_call(dev, "SetTextureStageState", &[0, 16, filter]),
            0
        );
        assert_eq!(
            f.device_call(dev, "SetTextureStageState", &[0, 17, filter]),
            0
        );
        assert_eq!(
            f.device_call(dev, "Clear", &[0, 0, 1, 0xff00ff00, 1f32.to_bits(), 0]),
            0
        );
        draw_tl(&mut f, dev, 5, &quad(0., 0., 64., 4., u32::MAX));
        let expected = EXPECTATIONS
            .lines()
            .find_map(|line| {
                let prefix = format!("colorkey\thal\t{mode}\tdraw\t00000000\t");
                line.strip_prefix(&prefix)
            })
            .unwrap();
        let expected = (0..expected.len())
            .step_by(6)
            .map(|i| u32::from_str_radix(&expected[i..i + 6], 16).unwrap())
            .collect::<Vec<_>>();
        let actual = f
            .pixels(rt, 64, 3)
            .into_iter()
            .skip(128)
            .step_by(2)
            .collect::<Vec<_>>();
        if i == 3 {
            for (actual, expected) in actual.into_iter().zip(expected) {
                for shift in [0, 8, 16] {
                    assert!(((actual >> shift) & 255).abs_diff((expected >> shift) & 255) <= 1);
                }
            }
        } else {
            assert_eq!(actual, expected, "{mode}");
        }
    }
}

fn tl(x: f32, y: f32, z: f32, color: u32) -> [u32; 8] {
    [
        x.to_bits(),
        y.to_bits(),
        z.to_bits(),
        1f32.to_bits(),
        color,
        0,
        0,
        0,
    ]
}

// spec: GFX-D3D7-2, GFX-D3D7-3
#[test]
fn rgb_value_rules_share_the_hal_raster_path() {
    use adv_abi::types::D3DDEVICEDESC7;
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        32,
        32,
    );
    let texture = f.surface(dd, true, creation::TEXTURE, 4, 4);
    let (address, pitch) = f.lock(texture, 0);
    for y in 0..4 {
        f.write(
            address + y * pitch,
            bytemuck::cast_slice(&[0x80808080u32; 4]),
        );
    }
    f.unlock(texture);
    for (kind, label, ops, max) in [
        (caps::Device::Hal, "hal", 0x039effff, 16384),
        (caps::Device::Rgb, "rgb", 0x78ff, 1024),
    ] {
        let dev = f.device_kind(d3d, Face::Direct3D7, rt, kind);
        assert_eq!(f.device_call(dev, "GetCaps", &[DATA + 0x600]), 0);
        let desc: D3DDEVICEDESC7 = adv_abi::GuestPtr::new(DATA + 0x600)
            .read(&f.machine.cpu)
            .unwrap();
        assert_eq!(
            desc.dwMaxTextureWidth,
            max.min(
                f.machine
                    .ext
                    .get::<SurfaceStore>()
                    .device()
                    .caps()
                    .limits
                    .max_image_2d
            )
        );
        assert_eq!(desc.dwTextureOpCaps, ops);
        assert_eq!(f.device_call(dev, "SetRenderState", &[22, 1]), 0);
        assert_eq!(
            f.device_call(dev, "Clear", &[0, 0, 1, 0, 1f32.to_bits(), 0]),
            0
        );
        draw_tl(
            &mut f,
            dev,
            2,
            &[tl(1., 1., 0.5, u32::MAX), tl(6., 4., 0.5, u32::MAX)],
        );
        draw_tl(
            &mut f,
            dev,
            1,
            &[tl(1.5, 6.5, 0.5, u32::MAX), tl(3., 6., 0.5, u32::MAX)],
        );
        let fixture = "line-1,1-6,4-points-1.5,6.5-3,6";
        let expected = (0..8)
            .flat_map(|y| pixel_row("px", label, fixture, &y.to_string()))
            .collect::<Vec<_>>();
        assert_eq!(
            f.pixels(rt, 8, 8),
            expected,
            "{label} line and point coverage"
        );
        assert_eq!(f.device_call(dev, "SetTexture", &[0, texture]), 0);
        assert_eq!(f.device_call(dev, "SetTextureStageState", &[0, 1, 10]), 0);
        assert_eq!(f.device_call(dev, "SetTextureStageState", &[0, 3, 0]), 0);
        assert_eq!(f.device_call(dev, "ValidateDevice", &[OUT]), 0);
        assert_eq!(f.word(OUT), 1);
        draw_tl(&mut f, dev, 5, &quad(0., 0., 4., 4., 0x40404040));
        assert_eq!(
            f.pixels(rt, 1, 1),
            pixel_row("px", label, "subtract-half-0x40", "0")
        );
        assert_eq!(f.device_call(dev, "SetTextureStageState", &[0, 2, 3]), 0);
        if kind == caps::Device::Rgb {
            assert_eq!(f.device_call(dev, "ValidateDevice", &[OUT]), 0x8876081a);
            draw_tl(&mut f, dev, 5, &quad(0., 0., 4., 4., 0xff123456));
            assert_eq!(f.pixels(rt, 1, 1), [0x123456]);
        } else {
            assert_eq!(f.device_call(dev, "ValidateDevice", &[OUT]), 0);
        }
    }
}

fn pixel_format(flags: u32, bits: u32, masks: [u32; 4]) -> [u8; 32] {
    bytemuck::cast([32, flags, 0, bits, masks[0], masks[1], masks[2], masks[3]])
}

// spec: GFX-D3D7-2, GFX-D3D7-5
#[test]
fn packed_render_targets_use_hal_shading_and_dithering_rules() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.described_surface(
        dd,
        creation::Described {
            flags: creation::DDSD_CAPS
                | creation::DDSD_WIDTH
                | creation::DDSD_HEIGHT
                | creation::DDSD_PIXELFORMAT,
            caps: [
                creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
                0,
                0,
                0,
            ],
            width: 32,
            height: 32,
            format: pixel_format(0x40, 16, [0xf800, 0x7e0, 0x1f, 0]),
            ..Default::default()
        },
    );
    let mut vertices = quad(0., 0., 16., 16., 0);
    for (vertex, color) in vertices
        .iter_mut()
        .zip([0xff000000, 0xffff0000, 0xff0000ff, 0xff00ff00])
    {
        vertex[4] = color;
    }
    for kind in [caps::Device::Hal, caps::Device::Rgb] {
        let dev = f.device_kind(d3d, Face::Direct3D7, rt, kind);
        assert_eq!(f.device_call(dev, "SetRenderState", &[22, 1]), 0);
        for (shade, name) in [(1, "rt16-flat-dither0"), (2, "rt16-gouraud-dither0")] {
            assert_eq!(f.device_call(dev, "SetRenderState", &[9, shade]), 0);
            for dither in [0, 1] {
                assert_eq!(f.device_call(dev, "SetRenderState", &[26, dither]), 0);
                assert_eq!(
                    f.device_call(dev, "Clear", &[0, 0, 1, 0, 1f32.to_bits(), 0]),
                    0
                );
                draw_tl(&mut f, dev, 5, &vertices);
                let (address, pitch) = f.lock(rt, 0x10);
                for y in 0..16 {
                    let actual = (0..16)
                        .map(|x| {
                            let pair = f.word(address + y * pitch + (x & !1) * 2);
                            (pair >> ((x & 1) * 16)) & 0xffff
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(
                        actual,
                        pixel_row("packed", "hal", name, &y.to_string()),
                        "{kind:?} {name} dither {dither} row {y}"
                    );
                }
                f.unlock(rt);
            }
        }
    }
}

// spec: GFX-D3D7-4, GFX-D3D7-5
#[test]
fn packed_texture_keys_and_mip_levels_share_the_surface_store() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        16,
        16,
    );
    for (kind, label) in [(caps::Device::Hal, "hal"), (caps::Device::Rgb, "rgb")] {
        let dev = f.device_kind(d3d, Face::Direct3D7, rt, kind);
        assert_eq!(f.device_call(dev, "SetRenderState", &[22, 1]), 0);
        assert_eq!(f.device_call(dev, "SetRenderState", &[41, 1]), 0);
        assert_eq!(f.device_call(dev, "SetTextureStageState", &[0, 1, 2]), 0);
        for (name, bits, alpha, mips, key) in [
            ("xrgb32-nomip", 32, false, false, 0x00ff00ff),
            ("argb32-nomip", 32, true, false, 0x00ff00ff),
            ("rgb565-nomip", 16, false, false, 0xf81f),
            ("argb1555-nomip-key-color-bits", 16, true, false, 0x7c1f),
            ("argb1555-nomip-key-with-alpha", 16, true, false, 0xfc1f),
            (
                "argb32-nomip-key-with-alpha",
                32,
                true,
                false,
                0xffff00ffu32,
            ),
            ("xrgb32-mip-key-top", 32, false, true, 0x00ff00ff),
            ("xrgb32-mip-key-every-level", 32, false, true, 0x00ff00ff),
            ("argb32-mip-key-top", 32, true, true, 0x00ff00ff),
            ("argb32-mip-key-with-alpha", 32, true, true, 0xffff00ffu32),
            ("rgb565-mip-key-top", 16, false, true, 0xf81f),
        ] {
            let flags = 0x40 | u32::from(alpha);
            let (masks, magenta) = match (bits, alpha) {
                (16, false) => ([0xf800, 0x7e0, 0x1f, 0], 0xf81fu32),
                (16, true) => ([0x7c00, 0x3e0, 0x1f, 0x8000], 0xfc1f),
                (_, false) => ([0xff0000, 0xff00, 0xff, 0], 0xffff00ff),
                (_, true) => ([0xff0000, 0xff00, 0xff, 0xff000000], 0xffff00ff),
            };
            let mip_flags = if mips {
                creation::MIPMAP | creation::COMPLEX
            } else {
                0
            };
            let tex = f.described_surface(
                dd,
                creation::Described {
                    flags: 0x1007 | if mips { creation::DDSD_MIPMAPCOUNT } else { 0 },
                    width: 8,
                    height: 8,
                    mipmaps: 3,
                    caps: [creation::TEXTURE | mip_flags, 0, 0, 0],
                    format: pixel_format(flags, bits, masks),
                    ..Default::default()
                },
            );
            let mut level = tex;
            let count = if mips { 3 } else { 1 };
            let step = bits / 8;
            for n in 0..count {
                let (address, pitch) = f.lock(level, 0);
                let side = 8 >> n;
                for y in 0..side {
                    for x in 0..side {
                        let color = if n != 0 || x < side / 2 {
                            magenta
                        } else {
                            u32::MAX
                        };
                        f.write(
                            address + pitch * y + x * step,
                            &color.to_le_bytes()[..step as usize],
                        );
                    }
                }
                f.unlock(level);
                if n == 0 || name.ends_with("every-level") {
                    f.write(DATA + 0x180, bytemuck::bytes_of(&[key; 2]));
                    let hr = f.call(
                        "IDirectDrawSurface7::SetColorKey",
                        &[level, 8, DATA + 0x180],
                    );
                    let position = if n == 0 { "top" } else { "sublevel" };
                    let prefix = format!("setkey\t{label}\t{name}\t{position}\t");
                    let expected = EXPECTATIONS
                        .lines()
                        .find_map(|line| line.strip_prefix(&prefix))
                        .unwrap();
                    assert_eq!(hr, u32::from_str_radix(expected, 16).unwrap());
                }
                if n < count - 1 {
                    f.write(
                        DATA + 0x180,
                        bytemuck::bytes_of(&[creation::TEXTURE | creation::MIPMAP, 0, 0, 0]),
                    );
                    let next = f.output(
                        "IDirectDrawSurface7::GetAttachedSurface",
                        &[level, DATA + 0x180, OUT],
                    );
                    if level != tex {
                        f.call("IDirectDrawSurface7::Release", &[level]);
                    }
                    level = next;
                }
            }
            if level != tex {
                f.call("IDirectDrawSurface7::Release", &[level]);
            }
            assert_eq!(f.device_call(dev, "SetTexture", &[0, tex]), 0);
            assert_eq!(
                f.device_call(
                    dev,
                    "SetTextureStageState",
                    &[0, 18, if mips { 2 } else { 1 }]
                ),
                0
            );
            for pass in 0..if mips { 2 } else { 1 } {
                assert_eq!(
                    f.device_call(dev, "Clear", &[0, 0, 1, 0xff00ff00, 1f32.to_bits(), 0]),
                    0
                );
                let size = if pass == 0 { 8. } else { 4. };
                draw_tl(&mut f, dev, 5, &quad(0., 0., size, size, u32::MAX));
                let values = f
                    .pixels(rt, 8, 2)
                    .into_iter()
                    .skip(8)
                    .map(|px| {
                        if px == 0x00ff00 {
                            b'g'
                        } else if px & 0xff00 != 0 {
                            b'W'
                        } else {
                            b'M'
                        }
                    })
                    .collect::<Vec<_>>();
                let row = if pass == 0 {
                    "level0-left-key"
                } else {
                    "level1-all-key"
                };
                let prefix = format!("ck\t{label}\t{name}\t{row}\t");
                let expected = EXPECTATIONS
                    .lines()
                    .find_map(|line| line.strip_prefix(&prefix))
                    .unwrap()
                    .as_bytes();
                for x in 0..8 {
                    if kind == caps::Device::Rgb && x == 4 {
                        // Windows RGB includes the far edge. This front uses HAL coverage.
                        continue;
                    }
                    assert_eq!(values[x], expected[x], "{label} {name} {row} at x={x}");
                }
            }
            assert_eq!(f.device_call(dev, "SetTexture", &[0, 0]), 0);
            assert_eq!(f.call("IDirectDrawSurface7::Release", &[tex]), 0);
        }
    }
}

// spec: GFX-D3D7-3
#[test]
fn fixed_vertex_lighting_matches_windows_and_the_scalar_stage() {
    use super::object::Object;
    use crate::fixed::tnl;
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        32,
        32,
    );
    let dev = f.device(d3d, Face::Direct3D7, rt);
    let p: [f32; 16] = [
        1. / 16.,
        0.,
        0.,
        0.,
        0.,
        -1. / 16.,
        0.,
        0.,
        0.,
        0.,
        1.,
        0.,
        -1.,
        1.,
        0.,
        1.,
    ];
    f.write(DATA + 0x200, bytemuck::bytes_of(&p));
    assert_eq!(f.device_call(dev, "SetTransform", &[3, DATA + 0x200]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[141, 0]), 0);
    let mut material = [0f32; 17];
    material[..4].fill(1.);
    f.write(DATA + 0x200, bytemuck::bytes_of(&material));
    assert_eq!(f.device_call(dev, "SetMaterial", &[DATA + 0x200]), 0);
    let mut light = [0f32; 26];
    light[0] = f32::from_bits(3);
    light[1..5].fill(1.);
    light[18] = 1.;
    f.write(DATA + 0x200, bytemuck::bytes_of(&light));
    assert_eq!(f.device_call(dev, "SetLight", &[0, DATA + 0x200]), 0);
    assert_eq!(f.device_call(dev, "LightEnable", &[0, 1]), 0);
    let vertices = std::array::from_fn::<_, 16, _>(|i| {
        let angle = (i as f32 * 6.).to_radians();
        [
            2. * i as f32 + 0.25,
            0.25,
            0.5,
            angle.sin(),
            0.,
            -angle.cos(),
            f32::from_bits(u32::MAX),
            0.,
            0.,
        ]
    });
    f.write(DATA + 0x200, bytemuck::cast_slice(&vertices));
    assert_eq!(f.device_call(dev, "BeginScene", &[]), 0);
    assert_eq!(
        f.device_call(dev, "DrawPrimitive", &[1, 0x152, DATA + 0x200, 16, 0]),
        0
    );
    assert_eq!(f.device_call(dev, "EndScene", &[]), 0);
    let pixels = f
        .pixels(rt, 32, 1)
        .into_iter()
        .step_by(2)
        .collect::<Vec<_>>();
    let prefix = "vx\thal\tdirectional-diffuse-6deg-steps\t";
    let expected = EXPECTATIONS
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .unwrap();
    let expected = (0..expected.len())
        .step_by(6)
        .map(|i| u32::from_str_radix(&expected[i..i + 6], 16).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(pixels, expected);
    let draw = &f.machine.ext.get::<front::State>().draw;
    let Some(Object::Device(device)) = draw.com.unwrap().get::<Object>(&f.machine.sys.objects, dev)
    else {
        panic!()
    };
    let state = &device.state.as_ref().unwrap().fixed;
    let active = tnl::active(state).unwrap();
    let normal = tnl::normal_matrix(state);
    for (v, px) in vertices.iter().zip(pixels) {
        let (color, _) = tnl::illuminate(
            state,
            &active,
            [v[0], v[1], v[2], 1.],
            [v[3], v[4], v[5]],
            &normal,
            None,
            None,
        );
        assert_eq!((color[0] * 255.).round() as u32, px & 255);
    }
}

// spec: GFX-D3D7-2, GFX-D3D7-7
#[test]
fn registry_enumerates_device_classes_and_depth_formats_in_measured_order() {
    use adv_abi::types::{D3DDEVICEDESC7, DDPIXELFORMAT};
    let mut f = Fixture::new(true);
    let callback = DATA + 0xf00;
    f.machine.cpu.protect(DATA, 4096, Prot::RWX).unwrap();
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        2049,
        16,
    );
    let mut devices = Vec::new();
    let mut descriptors = Vec::new();
    assert_eq!(
        f.invoke(
            "IDirect3D7::EnumDevices",
            &[d3d, callback, DATA],
            |cpu, args| {
                assert_eq!(args[3], DATA);
                let name = cpu.read_cstr(args[1], 40).unwrap();
                let desc: D3DDEVICEDESC7 = adv_abi::GuestPtr::new(args[2]).read(cpu).unwrap();
                descriptors.push(desc);
                devices.push((
                    String::from_utf8(name).unwrap(),
                    bytemuck::cast::<_, [u8; 16]>(desc.deviceGUID),
                ));
                1
            }
        ),
        0
    );
    let expected = [caps::Device::Rgb, caps::Device::Hal, caps::Device::TnlHal];
    assert_eq!(
        devices,
        expected.map(|device| (device.name().0.to_owned(), device.guid()))
    );
    for (index, (device, label)) in [
        (caps::Device::Rgb, "rgb"),
        (caps::Device::Hal, "hal"),
        (caps::Device::TnlHal, "tnlhal"),
    ]
    .into_iter()
    .enumerate()
    {
        let desc = &descriptors[index];
        let capture = EXPECTATIONS
            .lines()
            .filter(|line| line.starts_with("device7\t"))
            .nth(index)
            .unwrap()
            .split('\t')
            .collect::<Vec<_>>();
        let measured = |field: &str| {
            let i = capture.iter().position(|&name| name == field).unwrap();
            u32::from_str_radix(capture[i + 1], 16).unwrap()
        };
        let rgb = device == caps::Device::Rgb;
        let triangle = &desc.dpcTriCaps;
        for (field, value, omitted) in [
            ("devcaps", desc.dwDevCaps, if rgb { 0x10 } else { 0x30 }),
            (
                "texcaps",
                triangle.dwTextureCaps,
                if rgb { 0x82 } else { 0x880 },
            ),
            (
                "filter",
                triangle.dwTextureFilterCaps,
                if rgb { 0 } else { 0x400 },
            ),
            ("blendsrc", triangle.dwSrcBlendCaps, 0),
            ("blenddst", triangle.dwDestBlendCaps, 0),
            (
                "raster",
                triangle.dwRasterCaps,
                if rgb { 6 } else { 0x21800 },
            ),
            ("zcmp", triangle.dwZCmpCaps, 0),
            ("alphacmp", triangle.dwAlphaCmpCaps, 0),
            ("shade", triangle.dwShadeCaps, 0),
            ("texblend", triangle.dwTextureBlendCaps, 0),
            ("fvfcaps", desc.dwFVFCaps, 0),
            (
                "texop",
                desc.dwTextureOpCaps,
                if rgb { 0 } else { 0x600000 },
            ),
            ("vpcaps", desc.dwVertexProcessingCaps, 0),
        ] {
            assert_eq!(value, measured(field) & !omitted, "{label} {field}");
        }
        assert_eq!(desc.dwDeviceRenderBitDepth & 0x400, 0x400);
        assert_eq!(desc.dwMaxActiveLights, 8);
        let target = f.surface(
            dd,
            true,
            creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
            16,
            16,
        );
        let dev = f.device_kind(d3d, Face::Direct3D7, target, device);
        assert_eq!(f.device_call(dev, "GetCaps", &[DATA + 0x600]), 0);
        let returned: D3DDEVICEDESC7 = adv_abi::GuestPtr::new(DATA + 0x600)
            .read(&f.machine.cpu)
            .unwrap();
        assert_eq!(bytemuck::bytes_of(&returned), bytemuck::bytes_of(desc));
        if device == caps::Device::Hal {
            let mut texture_formats = Vec::new();
            assert_eq!(
                f.invoke("IDirect3DDevice7::EnumTextureFormats", &[dev, callback, DATA], |cpu, args| {
                    let pf: DDPIXELFORMAT = adv_abi::GuestPtr::new(args[0]).read(cpu).unwrap();
                    let [_, flags, fourcc, bits, r, g, b, a]: [u32; 8] = bytemuck::cast(pf);
                    texture_formats.push(format!(
                        "texformat\tflags\t{flags:08x}\tfourcc\t{fourcc:08x}\tbits\t{bits}\tmasks\t{r:08x}\t{g:08x}\t{b:08x}\t{a:08x}",
                    ));
                    1
                }),
                0
            );
            let expected = EXPECTATIONS
                .lines()
                .filter(|line| line.starts_with("texformat\t"))
                .filter(|line| {
                    !line.starts_with("texformat\tflags\t00000004")
                        && !line.contains("flags\t00080000")
                        && !line.contains("flags\t000c0000")
                })
                .collect::<Vec<_>>();
            assert_eq!(texture_formats, expected);
        }
        f.write(DATA, &device.guid());
        let mut formats = Vec::new();
        assert_eq!(
            f.invoke(
                "IDirect3D7::EnumZBufferFormats",
                &[d3d, DATA, callback, DATA],
                |cpu, args| {
                    let desc: DDPIXELFORMAT = adv_abi::GuestPtr::new(args[0]).read(cpu).unwrap();
                    formats.push(format!(
                        "zformat\t{label}\tflags\t{:08x}\tbits\t{}\tstencil\t{}\tzmask\t{:08x}",
                        desc.dwFlags,
                        desc.dwRGBBitCount,
                        desc.dwRBitMask,
                        u32::from_le_bytes(desc.Anonymous3)
                    ));
                    1
                }
            ),
            0
        );
        let prefix = format!("zformat\t{label}\t");
        assert_eq!(
            formats,
            EXPECTATIONS
                .lines()
                .filter(|line| line.starts_with(&prefix))
                .collect::<Vec<_>>()
        );
        if device != caps::Device::Rgb {
            assert_eq!(
                f.call("IDirect3D7::CreateDevice", &[d3d, DATA, rt, OUT]),
                0x88760082
            );
        }
    }
}

// spec: GFX-D3D7-3, GFX-D3D7-5
#[test]
fn indexed_strided_and_vertex_buffer_draws_capture_their_input() {
    use adv_abi::types::{D3DDP_PTRSTRIDE, D3DDRAWPRIMITIVESTRIDEDDATA, D3DVERTEXBUFFERDESC};
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        16,
        16,
    );
    let dev = f.device(d3d, Face::Direct3D7, rt);
    let v = quad(1., 1., 5., 5., 0xff123456);
    let data = DATA + 0x200;
    let indices = DATA + 0x300;
    let desc = DATA + 0x400;
    f.write(data, bytemuck::cast_slice(&v));
    f.write(indices, bytemuck::cast_slice(&[0u16, 1, 2, 3]));
    let stride = |offset| D3DDP_PTRSTRIDE {
        lpvData: data + offset,
        dwStride: 32,
    };
    let strided = D3DDRAWPRIMITIVESTRIDEDDATA {
        position: stride(0),
        diffuse: stride(16),
        specular: stride(20),
        textureCoords: std::array::from_fn(|_| stride(24)),
        ..Default::default()
    };
    f.write(desc, bytemuck::bytes_of(&strided));
    assert_eq!(f.device_call(dev, "SetRenderState", &[22, 1]), 0);
    assert_eq!(f.device_call(dev, "BeginScene", &[]), 0);
    assert_eq!(
        f.device_call(
            dev,
            "DrawIndexedPrimitiveStrided",
            &[5, 0x1c4, desc, 4, indices, 4, 0]
        ),
        0
    );
    f.write(data, &[0; 128]);
    assert_eq!(f.device_call(dev, "EndScene", &[]), 0);
    assert_eq!(f.pixels(rt, 2, 2), [0, 0, 0, 0x123456]);
    let buffer = D3DVERTEXBUFFERDESC {
        dwSize: 16,
        dwCaps: 0x800,
        dwFVF: 0x1c4,
        dwNumVertices: 4,
    };
    f.write(desc, bytemuck::bytes_of(&buffer));
    let vb = f.output("IDirect3D7::CreateVertexBuffer", &[d3d, desc, OUT, 0]);
    let vertex = f.output("IDirect3DVertexBuffer7::Lock", &[vb, 0, OUT, OUT + 4]);
    assert_eq!(f.word(OUT + 4), 128);
    assert_eq!(
        f.call("IDirect3DVertexBuffer7::Lock", &[vb, 0, OUT, 0]),
        0x8876080e
    );
    let v = quad(2., 2., 6., 6., 0xffabcdef);
    f.write(vertex, bytemuck::cast_slice(&v));
    assert_eq!(f.call("IDirect3DVertexBuffer7::Unlock", &[vb]), 0);
    assert_eq!(
        f.device_call(dev, "Clear", &[0, 0, 1, 0, 1f32.to_bits(), 0]),
        0
    );
    assert_eq!(f.device_call(dev, "BeginScene", &[]), 0);
    assert_eq!(
        f.device_call(dev, "DrawIndexedPrimitiveVB", &[5, vb, 0, 4, indices, 4, 0]),
        0
    );
    assert_eq!(f.device_call(dev, "EndScene", &[]), 0);
    let pixels = f.pixels(rt, 3, 3);
    assert_eq!(&pixels[..8], &[0; 8]);
    assert_eq!(pixels[8], 0xabcdef);
    assert_eq!(f.call("IDirect3DVertexBuffer7::Release", &[vb]), 0);
}

// spec: GFX-D3D7-3, GFX-D3D7-5
#[test]
fn render_target_and_texture_state_blocks_retain_surfaces() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let caps = creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE;
    let first = f.surface(dd, true, caps, 16, 16);
    let second = f.surface(dd, true, caps, 16, 16);
    let texture = f.surface(dd, true, creation::TEXTURE, 4, 4);
    let dev = f.device(d3d, Face::Direct3D7, first);
    assert_eq!(f.device_call(dev, "SetRenderTarget", &[second, 0]), 0);
    assert_eq!(f.call("IDirectDrawSurface7::Release", &[first]), 0);
    assert_eq!(f.device_call(dev, "GetRenderTarget", &[OUT]), 0);
    assert_eq!(f.word(OUT), second);
    assert_eq!(f.call("IDirectDrawSurface7::Release", &[second]), 2);
    assert_eq!(f.device_call(dev, "BeginStateBlock", &[]), 0);
    assert_eq!(f.device_call(dev, "SetTexture", &[0, texture]), 0);
    assert_eq!(f.device_call(dev, "GetTexture", &[0, OUT]), 0);
    assert_eq!(f.word(OUT), 0);
    assert_eq!(f.device_call(dev, "EndStateBlock", &[OUT]), 0);
    let block = f.word(OUT);
    assert_eq!(f.call("IDirectDrawSurface7::Release", &[texture]), 1);
    assert_eq!(f.device_call(dev, "ApplyStateBlock", &[block]), 0);
    assert_eq!(f.device_call(dev, "GetTexture", &[0, OUT]), 0);
    assert_eq!(f.word(OUT), texture);
    assert_eq!(f.call("IDirectDrawSurface7::Release", &[texture]), 2);
    assert_eq!(f.device_call(dev, "DeleteStateBlock", &[block]), 0);
    assert_eq!(f.device_call(dev, "SetTexture", &[0, 0]), 0);
    assert_eq!(f.query(texture, IID_SURFACE).0, INVALID);
}

// spec: GFX-D3D7-3, GFX-D3D7-5
#[test]
fn hal_integer_bias_and_stencil_use_the_attached_depth_surface() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        32,
        32,
    );
    let z = f.described_surface(
        dd,
        creation::Described {
            flags: 0x1007,
            width: 32,
            height: 32,
            caps: [creation::ZBUFFER, 0, 0, 0],
            format: pixel_format(0x4400, 32, [8, 0xffffff00, 0xff, 0]),
            ..Default::default()
        },
    );
    assert_eq!(
        f.call("IDirectDrawSurface7::AddAttachedSurface", &[rt, z]),
        0
    );
    let dev = f.device(d3d, Face::Direct3D7, rt);
    assert_eq!(f.device_call(dev, "SetRenderState", &[22, 1]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[23, 2]), 0);
    for (name, bias) in [
        ("z-coplanar-less", 0),
        ("z-coplanar-less-zbias1", 1),
        ("z-coplanar-less-zbias16", 16),
    ] {
        assert_eq!(
            f.device_call(dev, "Clear", &[0, 0, 7, 0xff202020, 1f32.to_bits(), 0]),
            0
        );
        assert_eq!(f.device_call(dev, "SetRenderState", &[47, 0]), 0);
        draw_tl(&mut f, dev, 5, &quad(0., 0., 4., 4., 0xff00ff00));
        assert_eq!(f.device_call(dev, "SetRenderState", &[47, bias]), 0);
        draw_tl(&mut f, dev, 5, &quad(0., 0., 4., 4., 0xffff0000));
        assert_eq!(f.pixels(rt, 1, 1), pixel_row("stage", "hal", name, "0"));
    }
    assert_eq!(
        f.device_call(dev, "Clear", &[0, 0, 7, 0xff202020, 1f32.to_bits(), 255]),
        0
    );
    assert_eq!(f.device_call(dev, "SetRenderState", &[7, 0]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[52, 1]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[55, 7]), 0);
    draw_tl(&mut f, dev, 5, &quad(0., 0., 4., 4., 0xff00ff00));
    assert_eq!(f.device_call(dev, "SetRenderState", &[57, 0]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[55, 1]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[56, 3]), 0);
    draw_tl(&mut f, dev, 5, &quad(0., 0., 4., 4., 0xff0000ff));
    assert_eq!(f.pixels(rt, 1, 1), [0x0000ff]);
}

// spec: GFX-D3D7-3, GFX-D3D7-5
#[test]
fn hal_texel_centers_and_shared_triangle_edges_match_windows() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        32,
        32,
    );
    let tex = f.surface(dd, true, creation::TEXTURE, 4, 4);
    let dev = f.device(d3d, Face::Direct3D7, rt);
    let (address, pitch) = f.lock(tex, 0);
    for y in 0..4 {
        let pixels = std::array::from_fn::<_, 4, _>(|x| {
            0xff000000u32 | ((x as u32 * 0x40 + 0x10) << 16) | ((y * 0x40 + 0x10) << 8)
        });
        f.write(address + y * pitch, bytemuck::cast_slice(&pixels));
    }
    f.unlock(tex);
    assert_eq!(f.device_call(dev, "SetRenderState", &[22, 1]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[27, 1]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[19, 2]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[20, 2]), 0);
    draw_tl(&mut f, dev, 5, &quad(0., 0., 8., 8., 0xff404040));
    let expected = (0..9)
        .flat_map(|y| pixel_row("px", "hal", "shared-edge-additive-0x40", &y.to_string()))
        .collect::<Vec<_>>();
    assert_eq!(f.pixels(rt, 9, 9), expected);
    assert_eq!(f.device_call(dev, "SetRenderState", &[27, 0]), 0);
    assert_eq!(f.device_call(dev, "SetTexture", &[0, tex]), 0);
    assert_eq!(f.device_call(dev, "SetTextureStageState", &[0, 1, 2]), 0);
    for (case, a, b, filter, wrap) in [
        ("tex-1to1-int-point", 0., 4., 1, false),
        ("tex-1to1-minus-half-point", -0.5, 3.5, 1, false),
        ("tex-1to1-int-linear", 0., 4., 2, false),
        ("tex-1to1-minus-half-linear", -0.5, 3.5, 2, false),
        ("tex-2x-int-linear", 0., 8., 2, false),
        ("tex-wrap-uv-minus-0.5-1.5", 0., 8., 1, true),
    ] {
        assert_eq!(
            f.device_call(dev, "Clear", &[0, 0, 1, 0, 1f32.to_bits(), 0]),
            0
        );
        for stage in [16, 17] {
            assert_eq!(
                f.device_call(dev, "SetTextureStageState", &[0, stage, filter]),
                0
            );
        }
        assert_eq!(
            f.device_call(
                dev,
                "SetTextureStageState",
                &[0, 12, if wrap { 1 } else { 3 }]
            ),
            0
        );
        let mut vertices = quad(a, a, b, b, u32::MAX);
        if wrap {
            for (i, vertex) in vertices.iter_mut().enumerate() {
                vertex[6] = (if i & 1 != 0 { 1.5f32 } else { -0.5 }).to_bits();
                vertex[7] = (if i & 2 != 0 { 1.5f32 } else { -0.5 }).to_bits();
            }
        }
        draw_tl(&mut f, dev, 5, &vertices);
        let expected = (0..8)
            .flat_map(|y| pixel_row("px", "hal", case, &y.to_string()))
            .collect::<Vec<_>>();
        assert_eq!(f.pixels(rt, 8, 8), expected, "{case}");
    }
}

// spec: GFX-D3D7-3
#[test]
fn xyzb1_follows_the_device_class_and_blends_world_matrices() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        32,
        32,
    );
    let projection: [f32; 16] = [
        1. / 16.,
        0.,
        0.,
        0.,
        0.,
        -1. / 16.,
        0.,
        0.,
        0.,
        0.,
        1.,
        0.,
        -1.,
        1.,
        0.,
        1.,
    ];
    let world1: [f32; 16] = [
        1., 0., 0., 0., 0., 1., 0., 0., 0., 0., 1., 0., 0., 4., 0., 1.,
    ];
    let vertices = std::array::from_fn::<_, 5, _>(|i| {
        [
            2. * i as f32 + 0.25,
            18.25,
            0.5,
            i as f32 / 4.,
            f32::from_bits(u32::MAX),
        ]
    });
    for (kind, label) in [
        (caps::Device::Hal, "hal"),
        (caps::Device::TnlHal, "tnlhal"),
        (caps::Device::Rgb, "rgb"),
    ] {
        let dev = f.device_kind(d3d, Face::Direct3D7, rt, kind);
        for (slot, matrix) in [(3, projection), (4, world1)] {
            f.write(DATA + 0x200, bytemuck::bytes_of(&matrix));
            assert_eq!(f.device_call(dev, "SetTransform", &[slot, DATA + 0x200]), 0);
        }
        f.write(DATA + 0x200, bytemuck::cast_slice(&vertices));
        assert_eq!(f.device_call(dev, "SetRenderState", &[137, 0]), 0);
        assert_eq!(f.device_call(dev, "SetRenderState", &[151, 1]), 0);
        assert_eq!(
            f.device_call(dev, "Clear", &[0, 0, 1, 0, 1f32.to_bits(), 0]),
            0
        );
        assert_eq!(f.device_call(dev, "BeginScene", &[]), 0);
        let hr = f.device_call(dev, "DrawPrimitive", &[1, 0x46, DATA + 0x200, 5, 0]);
        let prefix = format!("vx\t{label}\tvertex-blend-1weight-draw\t");
        let expected = EXPECTATIONS
            .lines()
            .find_map(|line| line.strip_prefix(&prefix))
            .unwrap();
        assert_eq!(hr, u32::from_str_radix(expected, 16).unwrap());
        assert_eq!(f.device_call(dev, "EndScene", &[]), 0);
        let pixels = f.pixels(rt, 10, 26);
        let rows = (0..5)
            .map(|i| {
                (16..26)
                    .find(|&y| pixels[y * 10 + 2 * i] != 0)
                    .map_or(-1, |y| y as i32)
            })
            .collect::<Vec<_>>();
        let prefix = format!("vx\t{label}\tvertex-blend-1weight-row-of-point-c=0..4\t");
        let expected = EXPECTATIONS
            .lines()
            .find_map(|line| line.strip_prefix(&prefix))
            .unwrap();
        assert_eq!(
            rows,
            expected
                .split('\t')
                .map(|v| v.parse::<i32>().unwrap())
                .collect::<Vec<_>>()
        );
    }
}

// spec: GFX-D3D7-6
#[test]
fn losing_the_render_surface_suppresses_clear_and_draw() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::PRIMARY | creation::THREE_D_DEVICE,
        32,
        32,
    );
    let dev = f.device(d3d, Face::Direct3D7, rt);
    let other = f.dd(7);
    assert_eq!(
        f.call("IDirectDraw7::SetCooperativeLevel", &[other, 1, 0x11]),
        0
    );
    assert_eq!(f.call("IDirectDrawSurface7::IsLost", &[rt]), 0x887601c2);
    assert_eq!(f.device_call(dev, "BeginScene", &[]), 0x887602fa);
    assert_eq!(
        f.device_call(dev, "Clear", &[0, 0, 1, u32::MAX, 1f32.to_bits(), 0]),
        0
    );
    let vertices = quad(0., 0., 4., 4., u32::MAX);
    f.write(DATA + 0x200, bytemuck::cast_slice(&vertices));
    assert_eq!(
        f.device_call(dev, "DrawPrimitive", &[5, 0x1c4, DATA + 0x200, 4, 0]),
        0
    );
    assert_eq!(f.device_call(dev, "SetTexture", &[0, 0]), 0);
    assert_eq!(f.device_call(dev, "EndScene", &[]), 0x887602f9);
    f.call("IDirectDraw7::Release", &[other]);
    assert_eq!(f.call("IDirectDrawSurface7::Restore", &[rt]), 0);
    assert_eq!(f.call("IDirectDrawSurface7::IsLost", &[rt]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[22, 1]), 0);
    assert_eq!(f.pixels(rt, 1, 1), [0]);
    draw_tl(&mut f, dev, 5, &vertices);
    assert_eq!(f.pixels(rt, 1, 1), [0xffffff]);
}

// spec: GFX-D3D7-3, GFX-D3D7-5
#[test]
fn camera_space_coordinates_feed_the_texture_matrix() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        32,
        32,
    );
    let dev = f.device(d3d, Face::Direct3D7, rt);
    let texture = f.surface(dd, true, creation::TEXTURE, 16, 1);
    let (at, _) = f.lock(texture, 0);
    let texels = std::array::from_fn::<_, 16, _>(|i| (i as u32 * 16 + 8) << 16 | 0x40);
    f.write(at, bytemuck::cast_slice(&texels));
    f.unlock(texture);
    let projection: [f32; 16] = [
        1. / 16.,
        0.,
        0.,
        0.,
        0.,
        -1. / 16.,
        0.,
        0.,
        0.,
        0.,
        1.,
        0.,
        -1.,
        1.,
        0.,
        1.,
    ];
    let uv: [f32; 16] = [
        1. / 32.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        0.,
        1.,
        0.,
        0.,
        0.,
        0.,
        1.,
    ];
    for (slot, matrix) in [(3, projection), (16, uv)] {
        f.write(DATA + 0x200, bytemuck::bytes_of(&matrix));
        assert_eq!(f.device_call(dev, "SetTransform", &[slot, DATA + 0x200]), 0);
    }
    assert_eq!(f.device_call(dev, "SetRenderState", &[137, 0]), 0);
    assert_eq!(f.device_call(dev, "SetTexture", &[0, texture]), 0);
    for (slot, value) in [(1, 2), (11, 0x20000), (24, 2)] {
        assert_eq!(
            f.device_call(dev, "SetTextureStageState", &[0, slot, value]),
            0
        );
    }
    let vertices = std::array::from_fn::<_, 16, _>(|i| [i as f32 * 2. + 0.25, 16.25, 0.5]);
    f.write(DATA + 0x200, bytemuck::cast_slice(&vertices));
    assert_eq!(f.device_call(dev, "BeginScene", &[]), 0);
    assert_eq!(
        f.device_call(dev, "DrawPrimitive", &[1, 2, DATA + 0x200, 16, 0]),
        0
    );
    assert_eq!(f.device_call(dev, "EndScene", &[]), 0);
    let pixels = f
        .pixels(rt, 32, 17)
        .into_iter()
        .skip(16 * 32)
        .step_by(2)
        .collect::<Vec<_>>();
    assert_eq!(
        pixels,
        pixel_row("stage", "hal", "texgen-cameraspaceposition-u=x/32", "0")
    );
}

// spec: GFX-D3D7-3, GFX-D3D7-5
#[test]
fn packed_colors_expand_before_combining_and_blending() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        16,
        16,
    );
    let dev = f.device(d3d, Face::Direct3D7, rt);
    assert_eq!(f.device_call(dev, "SetRenderState", &[22, 1]), 0);
    assert_eq!(f.device_call(dev, "SetTextureStageState", &[0, 1, 2]), 0);
    for (name, masks, value, blend) in [
        (
            "texture-565-0x8410-expanded",
            [0xf800, 0x7e0, 0x1f, 0],
            0x8410u16,
            false,
        ),
        (
            "texture-1555-0x4210-expanded",
            [0x7c00, 0x3e0, 0x1f, 0x8000],
            0x4210,
            false,
        ),
        (
            "texture-4444-0x8fff-srcalpha-over-404040",
            [0xf00, 0xf0, 0xf, 0xf000],
            0x8fff,
            true,
        ),
    ] {
        let tex = f.described_surface(
            dd,
            creation::Described {
                flags: 0x1007,
                width: 1,
                height: 1,
                caps: [creation::TEXTURE | creation::SYSTEM_MEMORY, 0, 0, 0],
                format: pixel_format(0x40 | u32::from(masks[3] != 0), 16, masks),
                ..Default::default()
            },
        );
        let (at, _) = f.lock(tex, 0);
        f.write(at, &value.to_le_bytes());
        f.unlock(tex);
        assert_eq!(
            f.device_call(dev, "Clear", &[0, 0, 1, 0xff404040, 1f32.to_bits(), 0]),
            0
        );
        assert_eq!(f.device_call(dev, "SetTexture", &[0, tex]), 0);
        assert_eq!(
            f.device_call(dev, "SetRenderState", &[27, u32::from(blend)]),
            0
        );
        if blend {
            assert_eq!(f.device_call(dev, "SetRenderState", &[19, 5]), 0);
            assert_eq!(f.device_call(dev, "SetRenderState", &[20, 6]), 0);
        }
        draw_tl(&mut f, dev, 5, &quad(0., 0., 8., 8., u32::MAX));
        assert_eq!(f.pixels(rt, 1, 1), pixel_row("stage", "hal", name, "0"));
    }
}

// spec: GFX-D3D7-5
#[test]
fn packed_targets_blend_color_and_return_nearest_packed_channels() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let caps = creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE | creation::VIDEO_MEMORY;
    for (masks, packed) in [
        ([0xf800, 0x7e0, 0x1f, 0], 0xa514u16),
        ([0x7c00, 0x3e0, 0x1f, 0], 0x5294),
        ([0x7c00, 0x3e0, 0x1f, 0x8000], 0xd294),
        ([0xf00, 0xf0, 0xf, 0xf000], 0xbaaa),
    ] {
        let rt = f.described_surface(
            dd,
            creation::Described {
                flags: 0x1007,
                width: 8,
                height: 8,
                caps: [caps, 0, 0, 0],
                format: pixel_format(0x40 | u32::from(masks[3] != 0), 16, masks),
                ..Default::default()
            },
        );
        let dev = f.device(d3d, Face::Direct3D7, rt);
        for (slot, value) in [(22, 1), (19, 5), (20, 6), (27, 1)] {
            assert_eq!(f.device_call(dev, "SetRenderState", &[slot, value]), 0);
        }
        assert_eq!(f.device_call(dev, "ValidateDevice", &[OUT]), 0);
        assert_eq!(f.word(OUT), 1);
        for dither in [0, 1] {
            assert_eq!(f.device_call(dev, "SetRenderState", &[26, dither]), 0);
            assert_eq!(
                f.device_call(dev, "Clear", &[0, 0, 1, 0xff404040, 1f32.to_bits(), 0]),
                0
            );
            draw_tl(&mut f, dev, 5, &quad(0., 0., 4., 4., 0x80ffffff));
            let (address, _) = f.lock(rt, 0x10);
            assert_eq!(f.word(address) as u16, packed, "{masks:x?}");
            f.unlock(rt);
        }
    }
}

// spec: GFX-D3D7-2
#[test]
fn textured_rgb_lines_emit_programmable_terminal_pixels_in_draw_order() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        8,
        8,
    );
    let tex = f.surface(dd, true, creation::TEXTURE, 1, 1);
    let (at, _) = f.lock(tex, 0);
    f.write(at, &0xff404040u32.to_le_bytes());
    f.unlock(tex);
    let dev = f.device_kind(d3d, Face::Direct3D7, rt, caps::Device::Rgb);
    assert_eq!(f.device_call(dev, "SetTexture", &[0, tex]), 0);
    for (slot, value) in [(19, 2), (20, 2), (27, 1)] {
        assert_eq!(f.device_call(dev, "SetRenderState", &[slot, value]), 0);
    }
    let vertices = [
        tl(1., 1., 0.5, u32::MAX),
        tl(5., 1., 0.5, u32::MAX),
        tl(5., 5., 0.5, u32::MAX),
    ];
    f.write(DATA + 0x200, bytemuck::cast_slice(&vertices));
    f.write(DATA + 0x300, bytemuck::cast_slice(&[0u16, 1, 2]));
    assert_eq!(f.device_call(dev, "BeginScene", &[]), 0);
    assert_eq!(
        f.device_call(
            dev,
            "DrawIndexedPrimitive",
            &[3, 0x1c4, DATA + 0x200, 3, DATA + 0x300, 3, 0]
        ),
        0
    );
    assert_eq!(f.device_call(dev, "EndScene", &[]), 0);
    let pixels = f.pixels(rt, 8, 8);
    for y in 0..8 {
        for x in 0..8 {
            let touched = y == 1 && (1..=5).contains(&x) || x == 5 && (2..=5).contains(&y);
            assert_eq!(
                pixels[y * 8 + x],
                if touched { 0x404040 } else { 0 },
                "{x},{y}"
            );
        }
    }
}

// spec: GFX-D3D7-3
#[test]
fn device7_applies_the_captured_hal_combiner_blend_and_alpha_test_rules() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        8,
        8,
    );
    let dev = f.device(d3d, Face::Direct3D7, rt);
    let white = f.surface(dd, true, creation::TEXTURE, 1, 1);
    let half = f.surface(dd, true, creation::TEXTURE, 1, 1);
    for (tex, value) in [(white, u32::MAX), (half, 0x80808080)] {
        let (at, _) = f.lock(tex, 0);
        f.write(at, &value.to_le_bytes());
        f.unlock(tex);
    }
    assert_eq!(f.device_call(dev, "SetRenderState", &[22, 1]), 0);
    for (case, op, texture, diffuse) in [
        ("modulate-white-tex-white-diffuse", 4, white, u32::MAX),
        ("modulate-half-half", 4, half, 0x80808080),
        ("modulate2x-half-half", 5, half, 0x80808080),
        ("modulate4x-half-half", 6, half, 0x80808080),
        ("add-half-half", 7, half, 0x80808080),
        ("addsigned-half-half", 8, half, 0x80808080),
        ("subtract-half-0x40", 10, half, 0x40404040),
        ("blenddiffusealpha-a40", 12, white, 0x40000000),
        ("selectarg2-diffuse-0x7f", 3, half, 0x7f7f7f7f),
    ] {
        assert_eq!(f.device_call(dev, "SetTexture", &[0, texture]), 0);
        assert_eq!(f.device_call(dev, "SetTextureStageState", &[0, 1, op]), 0);
        draw_tl(&mut f, dev, 5, &quad(0., 0., 4., 4., diffuse));
        assert_eq!(
            f.pixels(rt, 1, 1),
            pixel_row("px", "hal", case, "0"),
            "{case}"
        );
    }
    assert_eq!(f.device_call(dev, "SetTexture", &[0, 0]), 0);
    assert_eq!(f.device_call(dev, "SetTextureStageState", &[0, 1, 1]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[27, 1]), 0);
    for (case, src, dst, color) in [
        ("blend-srcalpha-a80-white-over-0x404040", 5, 6, 0x80ffffff),
        ("blend-srcalpha-a7f-white-over-0x404040", 5, 6, 0x7fffffff),
        ("blend-srcalpha-a01-white-over-0x404040", 5, 6, 0x01ffffff),
        ("blend-one-one-0xc0-over-0x404040", 2, 2, 0xffc0c0c0),
        ("blend-destcolor-zero-0x80-over-0x404040", 9, 1, 0xff808080),
        (
            "blend-bothsrcalpha-a40-white-over-0x404040",
            12,
            1,
            0x40ffffff,
        ),
    ] {
        assert_eq!(
            f.device_call(dev, "Clear", &[0, 0, 1, 0xff404040, 1f32.to_bits(), 0]),
            0
        );
        assert_eq!(f.device_call(dev, "SetRenderState", &[19, src]), 0);
        assert_eq!(f.device_call(dev, "SetRenderState", &[20, dst]), 0);
        draw_tl(&mut f, dev, 5, &quad(0., 0., 4., 4., color));
        assert_eq!(
            f.pixels(rt, 1, 1),
            pixel_row("px", "hal", case, "0"),
            "{case}"
        );
    }
    for (slot, value) in [(27, 0), (15, 1), (25, 7)] {
        assert_eq!(f.device_call(dev, "SetRenderState", &[slot, value]), 0);
    }
    for reference in [0x7f, 0x80, 0x81] {
        assert_eq!(
            f.device_call(dev, "Clear", &[0, 0, 1, 0, 1f32.to_bits(), 0]),
            0
        );
        assert_eq!(f.device_call(dev, "SetRenderState", &[24, reference]), 0);
        draw_tl(&mut f, dev, 5, &quad(0., 0., 4., 4., 0x80ffffff));
        let case = format!("alphatest-a80-gequal-ref{reference:02x}");
        assert_eq!(f.pixels(rt, 1, 1), pixel_row("px", "hal", &case, "0"));
    }
}

// spec: GFX-D3D7-3
#[test]
fn device7_admits_documented_raster_states_and_rejects_retired_selectors() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        8,
        8,
    );
    let dev = f.device(d3d, Face::Direct3D7, rt);
    for slot in [
        1, 3, 5, 6, 11, 12, 13, 17, 18, 21, 30, 31, 32, 39, 43, 44, 45, 46, 49, 50, 51,
    ]
    .into_iter()
    .chain(64..=95)
    {
        f.write(OUT, &0xdead_beefu32.to_le_bytes());
        assert_eq!(
            f.device_call(dev, "GetRenderState", &[slot, OUT]),
            INVALID,
            "{slot}"
        );
        assert_eq!(f.word(OUT), 0xdead_beef);
        assert_eq!(
            f.device_call(dev, "SetRenderState", &[slot, 1]),
            INVALID,
            "{slot}"
        );
    }
    for (slot, value) in [
        (2, 2),
        (7, 2),
        (9, 3),
        (10, 0x55550001),
        (40, 1),
        (47, 17),
        (128, 16),
    ] {
        assert_eq!(
            f.device_call(dev, "SetRenderState", &[slot, value]),
            INVALID
        );
    }
    for (slot, value) in [(22, 1), (8, 1), (138, 1), (135, 15)] {
        assert_eq!(f.device_call(dev, "SetRenderState", &[slot, value]), 0);
        assert_eq!(f.device_call(dev, "GetRenderState", &[slot, OUT]), 0);
        assert_eq!(f.word(OUT), value);
    }
    let triangle = [
        tl(1., 1., 0.5, u32::MAX),
        tl(5., 1., 0.5, u32::MAX),
        tl(1., 5., 0.5, u32::MAX),
    ];
    draw_tl(&mut f, dev, 4, &triangle);
    let points = f.pixels(rt, 8, 8);
    assert_eq!(points.iter().filter(|&&value| value == 0xffffff).count(), 3);
    assert_eq!([points[9], points[13], points[41]], [0xffffff; 3]);
    assert_eq!(f.device_call(dev, "GetClipStatus", &[DATA + 0x700]), 0);
    assert_eq!(
        std::array::from_fn(|i| f.word(DATA + 0x708 + i as u32 * 4)),
        [1f32, 5., 1., 5., 0.5, 0.5].map(f32::to_bits)
    );
    assert_eq!(f.device_call(dev, "SetRenderState", &[8, 2]), 0);
    assert_eq!(
        f.device_call(dev, "Clear", &[0, 0, 1, 0, 1f32.to_bits(), 0]),
        0
    );
    draw_tl(&mut f, dev, 4, &triangle);
    let lines = f.pixels(rt, 8, 8);
    assert_eq!([lines[11], lines[25], lines[27]], [0xffffff; 3]);
    assert_eq!(lines[18], 0);

    let texture = f.surface(dd, true, creation::TEXTURE, 4, 1);
    let (address, _) = f.lock(texture, 0);
    f.write(
        address,
        bytemuck::cast_slice(&[0xffff0000u32, 0xff00ff00, 0xff0000ff, 0xffffffff]),
    );
    f.unlock(texture);
    assert_eq!(f.device_call(dev, "SetTexture", &[0, texture]), 0);
    assert_eq!(f.device_call(dev, "SetTextureStageState", &[0, 1, 2]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[8, 3]), 0);
    let mut seam = quad(0., 0., 8., 8., u32::MAX);
    for (vertex, u) in seam.iter_mut().zip([0.875f32, 0.125, 0.875, 0.125]) {
        vertex[6] = u.to_bits();
    }
    for (wrap, expected) in [(0, 0x0000ff), (1, 0xff0000)] {
        assert_eq!(f.device_call(dev, "SetRenderState", &[128, wrap]), 0);
        draw_tl(&mut f, dev, 5, &seam);
        assert_eq!(f.pixels(rt, 8, 1)[4], expected);
    }
}

// spec: GFX-D3D7-5, GFX-BLT-1
#[test]
fn packed_target_blt_crosses_raw_and_color_residency_in_both_directions() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let packed = creation::Described {
        flags: 0x1007,
        caps: [creation::TEXTURE | creation::VIDEO_MEMORY, 0, 0, 0],
        width: 8,
        height: 8,
        format: pixel_format(0x40, 16, [0xf800, 0x7e0, 0x1f, 0]),
        ..Default::default()
    };
    let mut renderable = packed;
    renderable.caps[0] |= creation::THREE_D_DEVICE;
    let rt = f.described_surface(dd, renderable);
    let texture = f.described_surface(dd, packed);
    let output = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        8,
        8,
    );
    let dev = f.device(d3d, Face::Direct3D7, rt);
    let (address, pitch) = f.lock(texture, 0);
    for y in 0..8 {
        f.write(address + y * pitch, bytemuck::cast_slice(&[0x8410u16; 8]));
    }
    f.unlock(texture);
    assert_eq!(f.device_call(dev, "PreLoad", &[texture]), 0);
    assert_eq!(
        f.device_call(dev, "Clear", &[0, 0, 1, 0, 1f32.to_bits(), 0]),
        0
    );
    assert_eq!(
        f.call("IDirectDrawSurface7::Blt", &[rt, 0, texture, 0, 0, 0]),
        0
    );
    assert_eq!(f.device_call(dev, "SetRenderTarget", &[output, 0]), 0);
    assert_eq!(f.device_call(dev, "SetTexture", &[0, rt]), 0);
    assert_eq!(f.device_call(dev, "SetTextureStageState", &[0, 1, 2]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[22, 1]), 0);
    draw_tl(&mut f, dev, 5, &quad(0., 0., 8., 8., u32::MAX));
    assert_eq!(f.pixels(output, 1, 1), [0x848284]);
    let (at, _) = f.lock(rt, 0);
    f.write(at, &0xf800u16.to_le_bytes());
    f.unlock(rt);
    assert_eq!(f.device_call(dev, "PreLoad", &[rt]), 0);
    assert_eq!(
        f.call("IDirectDrawSurface7::Blt", &[texture, 0, rt, 0, 0, 0]),
        0
    );
    let (at, _) = f.lock(texture, 0x10);
    assert_eq!(f.word(at), 0x8410f800);
    f.unlock(texture);
}

// spec: GFX-D3D7-3
#[test]
fn device7_vertex_material_light_and_fog_rows_match_the_capture() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        32,
        32,
    );
    let dev = f.device(d3d, Face::Direct3D7, rt);
    let projection: [f32; 16] = [
        1. / 16.,
        0.,
        0.,
        0.,
        0.,
        -1. / 16.,
        0.,
        0.,
        0.,
        0.,
        1.,
        0.,
        -1.,
        1.,
        0.,
        1.,
    ];
    f.write(DATA + 0x200, bytemuck::bytes_of(&projection));
    assert_eq!(f.device_call(dev, "SetTransform", &[3, DATA + 0x200]), 0);
    let vertices = |y: u32| {
        std::array::from_fn::<_, 16, _>(|i| {
            [
                2. * i as f32 + 0.25,
                y as f32 + 0.25,
                0.5,
                0.,
                0.,
                -1.,
                f32::from_bits(u32::MAX),
                0.,
                0.,
            ]
        })
    };
    let compare = |f: &mut Fixture, case: &str, input: [[f32; 9]; 16], tolerance: u8| {
        f.write(DATA + 0x200, bytemuck::cast_slice(&input));
        assert_eq!(f.device_call(dev, "BeginScene", &[]), 0);
        assert_eq!(
            f.device_call(dev, "DrawPrimitive", &[1, 0x152, DATA + 0x200, 16, 0]),
            0
        );
        assert_eq!(f.device_call(dev, "EndScene", &[]), 0);
        // The runtime saturates one near-normal highlight that the T&L driver
        // and the scalar exponent evaluate to 0xfc.
        let device = if case == "specular-power10-nonlocal" {
            "tnlhal"
        } else {
            "hal"
        };
        let prefix = format!("vx\t{device}\t{case}\t");
        let expected = EXPECTATIONS
            .lines()
            .find_map(|line| line.strip_prefix(&prefix))
            .unwrap();
        let y = input[0][1].floor() as usize;
        let pixels = f.pixels(rt, 32, y as u32 + 1);
        for (i, word) in expected.as_bytes().as_chunks::<6>().0.iter().enumerate() {
            let expected = u32::from_str_radix(std::str::from_utf8(word).unwrap(), 16).unwrap();
            let actual = pixels[32 * y + i * 2];
            for channel in 0..3 {
                assert!(
                    actual.to_le_bytes()[channel].abs_diff(expected.to_le_bytes()[channel])
                        <= tolerance,
                    "{case} vertex {i}, got {actual:06x}, expected {expected:06x}"
                );
            }
        }
    };
    for (case, row, kind, position, material, ambient, specular, local, normalize, normal_length) in [
        (
            "directional-normal-length-0.5",
            1,
            3,
            [0., 0., 0.],
            [1., 0., 0., 0., 0.],
            0.,
            false,
            true,
            false,
            0.5,
        ),
        (
            "directional-normal-length-0.5-normalized",
            1,
            3,
            [0., 0., 0.],
            [1., 0., 0., 0., 0.],
            0.,
            false,
            true,
            true,
            0.5,
        ),
        (
            "ambient-global40-mat0.5-light0.25-diffuse0.5",
            2,
            3,
            [0., 0., 0.],
            [0.5, 0.5, 0., 0., 0.],
            0.25,
            false,
            true,
            false,
            1.,
        ),
        (
            "specular-power10-localviewer",
            3,
            3,
            [0., 0., 0.],
            [0., 0., 1., 0., 10.],
            0.,
            true,
            true,
            false,
            1.,
        ),
        (
            "specular-power10-nonlocal",
            4,
            3,
            [0., 0., 0.],
            [0., 0., 1., 0., 10.],
            0.,
            true,
            false,
            false,
            1.,
        ),
        (
            "point-light-16,5,-4-att0=1-att1=0.05",
            5,
            1,
            [16., 5., -4.],
            [1., 0., 0., 0., 0.],
            0.,
            false,
            true,
            false,
            1.,
        ),
        (
            "spot-light-16,6,-8-theta0.3-phi0.9",
            6,
            2,
            [16., 6., -8.],
            [1., 0., 0., 0., 0.],
            0.,
            false,
            true,
            false,
            1.,
        ),
        (
            "colorvertex-diffuse-source-emissive0.125",
            7,
            3,
            [0., 0., 0.],
            [1., 0., 0., 0.125, 0.],
            0.,
            false,
            true,
            false,
            1.,
        ),
    ] {
        let colorvertex = row == 7;
        for (slot, value) in [
            (137, 1),
            (141, u32::from(colorvertex)),
            (29, u32::from(specular)),
            (142, u32::from(local)),
            (143, u32::from(normalize)),
            (139, if row == 2 { 0x404040 } else { 0 }),
        ] {
            assert_eq!(f.device_call(dev, "SetRenderState", &[slot, value]), 0);
        }
        let [d, a, s, e, power] = material;
        let material: [f32; 17] = [d, d, d, 1., a, a, a, 1., s, s, s, 1., e, e, e, 1., power];
        f.write(DATA + 0x200, bytemuck::bytes_of(&material));
        assert_eq!(f.device_call(dev, "SetMaterial", &[DATA + 0x200]), 0);
        let mut light = [0f32; 26];
        light[0] = f32::from_bits(kind);
        light[1..5].fill(1.);
        light[5..8].fill(f32::from(u8::from(specular)));
        light[8] = 1.;
        light[9..12].fill(ambient);
        light[12] = 1.;
        light[13..16].copy_from_slice(&position);
        light[18] = f32::from(u8::from(kind != 1));
        light[19..26].copy_from_slice(&[
            1000.,
            1.,
            1.,
            if kind == 3 { 0. } else { 0.05 },
            0.,
            0.3,
            0.9,
        ]);
        f.write(DATA + 0x200, bytemuck::bytes_of(&light));
        assert_eq!(f.device_call(dev, "SetLight", &[0, DATA + 0x200]), 0);
        assert_eq!(f.device_call(dev, "LightEnable", &[0, 1]), 0);
        let mut input = vertices(row);
        for (i, vertex) in input.iter_mut().enumerate() {
            if row <= 4 {
                let angle = (i as f32 * if specular { 3. } else { 6. }).to_radians();
                vertex[3] = angle.sin() * normal_length;
                vertex[5] = -angle.cos() * normal_length;
            }
            if colorvertex {
                let red = i as u32 * 17;
                vertex[6] = f32::from_bits(0xff000080 | red << 16 | (255 - red) << 8);
            }
        }
        compare(&mut f, case, input, 0);
    }
    assert_eq!(f.device_call(dev, "SetRenderState", &[137, 0]), 0);
    assert_eq!(f.device_call(dev, "SetRenderState", &[29, 0]), 0);
    let mut input = vertices(8);
    for (i, vertex) in input.iter_mut().enumerate() {
        let i = i as u32;
        vertex[6] = f32::from_bits(0xff000000 | (i * 17) << 16 | (i * 16) << 8 | (255 - i * 3));
    }
    compare(&mut f, "unlit-passthrough", input, 0);
    for (i, (case, vertex, table, range)) in [
        ("vertex-fog-linear-0.2-0.8", 3, 0, 0),
        ("vertex-fog-exp-2", 1, 0, 0),
        ("vertex-fog-exp2-2", 2, 0, 0),
        ("vertex-fog-linear-range", 3, 0, 1),
        ("table-fog-linear-0.2-0.8", 0, 3, 0),
        ("table-fog-exp-2", 0, 1, 0),
        ("table-fog-exp2-2", 0, 2, 0),
    ]
    .into_iter()
    .enumerate()
    {
        for (slot, value) in [
            (28, 1),
            (34, 0xff0000),
            (35, table),
            (140, vertex),
            (48, range),
            (36, 0.2f32.to_bits()),
            (37, 0.8f32.to_bits()),
            (38, 2f32.to_bits()),
        ] {
            assert_eq!(f.device_call(dev, "SetRenderState", &[slot, value]), 0);
        }
        let mut input = vertices(9 + i as u32);
        for (k, vertex) in input.iter_mut().enumerate() {
            vertex[2] = k as f32 / 15.;
            vertex[6] = f32::from_bits(0xff0000ff);
        }
        // The Windows table and vertex exponential fog rows differ by one LSB.
        compare(&mut f, case, input, u8::from(matches!(table, 1 | 2)));
    }
}

// spec: GFX-D3D7-2, GFX-D3D7-3
#[test]
fn device7_validates_and_draws_the_captured_texture_cascades() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        16,
        16,
    );
    let texture = f.described_surface(
        dd,
        creation::Described {
            flags: 0x1007,
            width: 1,
            height: 1,
            caps: [creation::TEXTURE, 0, 0, 0],
            format: pixel_format(0x40, 16, [0xf800, 0x7e0, 0x1f, 0]),
            ..Default::default()
        },
    );
    let (at, _) = f.lock(texture, 0);
    f.write(at, &0x8410u16.to_le_bytes());
    f.unlock(texture);
    for (kind, device) in [
        (caps::Device::Hal, "hal"),
        (caps::Device::TnlHal, "tnlhal"),
        (caps::Device::Rgb, "rgb"),
    ] {
        let dev = f.device_kind(d3d, Face::Direct3D7, rt, kind);
        for (slot, value) in [(22, 1), (60, 0x00204060)] {
            assert_eq!(f.device_call(dev, "SetRenderState", &[slot, value]), 0);
        }
        assert_eq!(f.device_call(dev, "SetTexture", &[0, texture]), 0);
        for (case, stage0, stage1, bound) in [
            ("stage0-selectarg1-tfactor", [2, 3, 0], [1, 0, 0], false),
            ("stage0-add-texture-tfactor", [7, 2, 3], [1, 0, 0], false),
            ("stage1-selectarg1-current", [2, 2, 0], [2, 1, 3], false),
            ("stage1-selectarg2-tfactor", [2, 2, 0], [3, 1, 3], false),
            (
                "stage1-add-current-tfactor-texture1-bound",
                [2, 2, 0],
                [7, 1, 3],
                true,
            ),
            (
                "stage1-modulate-current-texture1",
                [2, 2, 0],
                [4, 1, 2],
                true,
            ),
            (
                "stage1-add-current-diffuse-0x204060",
                [2, 2, 0],
                [7, 1, 0],
                false,
            ),
        ] {
            for (stage, values) in [stage0, stage1].into_iter().enumerate() {
                for (slot, value) in values.into_iter().enumerate() {
                    assert_eq!(
                        f.device_call(
                            dev,
                            "SetTextureStageState",
                            &[stage as u32, slot as u32 + 1, value]
                        ),
                        0
                    );
                }
            }
            assert_eq!(
                f.device_call(dev, "SetTexture", &[1, if bound { texture } else { 0 }]),
                0
            );
            f.write(OUT, &0u32.to_le_bytes());
            let prefix = format!("stage\t{device}\t{case}\t0\t");
            let captured = EXPECTATIONS
                .lines()
                .find_map(|line| line.strip_prefix(&prefix))
                .unwrap();
            let fields = captured.split('\t').collect::<Vec<_>>();
            assert_eq!(
                f.device_call(dev, "ValidateDevice", &[OUT]),
                u32::from_str_radix(fields[2], 16).unwrap(),
                "{device} {case}"
            );
            assert_eq!(
                f.word(OUT),
                fields[3].parse::<u32>().unwrap(),
                "{device} {case}"
            );
            draw_tl(&mut f, dev, 5, &quad(0., 0., 8., 8., 0xff204060));
            let color_device = if device == "rgb" && case == "stage0-add-texture-tfactor" {
                // RGB's truncated 565 expansion is replaced by HAL bit replication.
                "hal"
            } else {
                device
            };
            assert_eq!(
                f.pixels(rt, 1, 1),
                pixel_row("stage", color_device, case, "0"),
                "{device} {case}"
            );
        }
    }
}

// spec: GFX-D3D7-1, GFX-D3D7-3, GFX-D3D7-5
#[test]
fn legacy_execute_buffer_copies_vertices_and_draws_the_captured_triangle() {
    use adv_abi::types::D3DEXECUTEDATA;
    let mut f = Fixture::new(true);
    let dd = f.dd(1);
    let d3d = f.face(dd, IID_IDIRECT3D);
    let target = f.surface(
        dd,
        false,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        32,
        32,
    );
    let device = f.face(target, IID_IDIRECT3DHALDEVICE);
    let viewport = f.output("IDirect3D::CreateViewport", &[d3d, OUT, 0]);
    assert_eq!(
        f.call("IDirect3DDevice::AddViewport", &[device, viewport]),
        0
    );
    let desc = DATA + 0x80;
    f.write(
        desc,
        bytemuck::cast_slice(&[
            44u32,
            0,
            0,
            32,
            32,
            16f32.to_bits(),
            16f32.to_bits(),
            1f32.to_bits(),
            1f32.to_bits(),
            0,
            1f32.to_bits(),
        ]),
    );
    assert_eq!(
        f.call("IDirect3DViewport3::SetViewport", &[viewport, desc]),
        0
    );

    f.write(desc, bytemuck::cast_slice(&[20u32, 1, 0, 1024, 0]));
    let buffer = f.output(
        "IDirect3DDevice::CreateExecuteBuffer",
        &[device, desc, OUT, 0],
    );
    assert_eq!(f.call("IDirect3DExecuteBuffer::Lock", &[buffer, desc]), 0);
    let data = f.word(desc + 16);
    assert_eq!(f.word(desc + 12), 1024);
    assert_eq!(f.word(desc + 8), 0);
    assert_eq!(
        f.call("IDirect3DExecuteBuffer::Lock", &[buffer, desc]),
        0x887602ca
    );
    let vertices = [
        tl(2., 2., 0., 0xff00ff00),
        tl(30., 2., 0., 0xff00ff00),
        tl(2., 30., 0., 0xff00ff00),
    ];
    f.write(data, bytemuck::cast_slice(&vertices));
    let instructions = [
        0x0001_1009u32,
        2,
        0,
        3,
        0, // COPY three TL vertices
        0x0001_0808,
        7,
        0, // disable Z
        0x0001_0803,
        1 << 16,
        0x0700_0002, // triangle
        11,
    ];
    f.write(data + 96, bytemuck::cast_slice(&instructions));
    let mut execute = D3DEXECUTEDATA {
        dwSize: 48,
        dwVertexCount: 3,
        dwInstructionOffset: 96,
        dwInstructionLength: size_of_val(&instructions) as u32,
        ..Default::default()
    };
    f.write(desc, bytemuck::bytes_of(&execute));
    assert_eq!(
        f.call("IDirect3DExecuteBuffer::SetExecuteData", &[buffer, desc]),
        0
    );
    assert_eq!(f.call("IDirect3DDevice::BeginScene", &[device]), 0);
    assert_eq!(
        f.call("IDirect3DDevice::Execute", &[device, buffer, viewport, 1]),
        0x887602ca
    );
    assert_eq!(f.call("IDirect3DExecuteBuffer::Unlock", &[buffer]), 0);
    assert_eq!(f.call("IDirect3DViewport3::Clear", &[viewport, 0, 0, 1]), 0);
    assert_eq!(
        f.call("IDirect3DDevice::Execute", &[device, buffer, viewport, 1]),
        0
    );
    assert_eq!(
        f.call("IDirect3DExecuteBuffer::GetExecuteData", &[buffer, desc]),
        0
    );
    f.machine
        .cpu
        .read(desc, bytemuck::bytes_of_mut(&mut execute))
        .unwrap();
    let extent = execute.dsStatus.drExtent;
    assert_eq!(
        (
            execute.dsStatus.dwFlags,
            extent.x1,
            extent.y1,
            extent.x2,
            extent.y2
        ),
        (0, 1, 1, 31, 31)
    );
    assert_eq!(f.call("IDirect3DDevice::EndScene", &[device]), 0);
    let pixels = f.pixels(target, 32, 32);
    assert_eq!(pixels[6 + 6 * 32], 0x00ff00);
    assert_eq!(pixels[28 + 28 * 32], 0);
    f.write(desc, &20u32.to_le_bytes());
    assert_eq!(f.call("IDirect3DExecuteBuffer::Lock", &[buffer, desc]), 0);
    assert_eq!(f.word(desc + 16), data);
    assert_eq!(f.call("IDirect3DExecuteBuffer::Unlock", &[buffer]), 0);
    assert_eq!(f.call("IDirect3DExecuteBuffer::Release", &[buffer]), 0);
}

// spec: GFX-D3D7-3
#[test]
fn vertex_buffers_process_positions_and_preserve_diffuse_and_texcoords() {
    let mut f = Fixture::new(true);
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    let rt = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        32,
        32,
    );
    let device = f.device(d3d, Face::Direct3D7, rt);
    let desc = DATA + 0xc0;
    let lock =
        |f: &mut Fixture, buffer| f.output("IDirect3DVertexBuffer7::Lock", &[buffer, 0, OUT, 0]);
    let src = {
        f.write(desc, bytemuck::cast_slice(&[16u32, 0x800, 0x1e2, 1]));
        f.output("IDirect3D7::CreateVertexBuffer", &[d3d, desc, OUT, 0])
    };
    let dst = {
        f.write(desc, bytemuck::cast_slice(&[16u32, 0x800, 0x1c4, 1]));
        f.output("IDirect3D7::CreateVertexBuffer", &[d3d, desc, OUT, 0])
    };
    let ptr = lock(&mut f, src);
    f.write(
        ptr,
        bytemuck::cast_slice(&[
            0f32.to_bits(),
            0.5f32.to_bits(),
            0.25f32.to_bits(),
            0,
            0xff123456,
            0xff000001,
            0.75f32.to_bits(),
            0.5f32.to_bits(),
        ]),
    );
    assert_eq!(f.call("IDirect3DVertexBuffer7::Unlock", &[src]), 0);
    assert_eq!(
        f.call(
            "IDirect3DVertexBuffer7::ProcessVertices",
            &[dst, 1, 0, 1, src, 0, device, 0]
        ),
        0
    );
    let ptr = lock(&mut f, dst);
    assert_eq!(
        std::array::from_fn::<_, 8, _>(|i| f.word(ptr + i as u32 * 4)),
        [
            16f32.to_bits(),
            8f32.to_bits(),
            0.25f32.to_bits(),
            1f32.to_bits(),
            0xff123456,
            0xff000001,
            0.75f32.to_bits(),
            0.5f32.to_bits(),
        ]
    );
    assert_eq!(f.call("IDirect3DVertexBuffer7::Unlock", &[dst]), 0);
}

// spec: GFX-D3D7-6
#[test]
fn exclusive_mode_changes_and_window_activation_lose_video_surfaces() {
    #[derive(Clone, Copy)]
    struct Active(bool);
    let mut f = Fixture::new(true);
    f.machine.ext.insert(Active(true));
    f.machine
        .ext
        .get_mut::<front::State>()
        .set_activation_provider(|ext, _| Some(ext.get::<Active>().0));
    let dd = f.dd(7);
    let d3d = f.face(dd, IID_IDIRECT3D7);
    assert_eq!(
        f.call("IDirectDraw7::SetCooperativeLevel", &[dd, 1, 0x11]),
        0
    );
    let primary = f.surface(
        dd,
        true,
        creation::PRIMARY | creation::THREE_D_DEVICE,
        32,
        32,
    );
    let video = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::VIDEO_MEMORY,
        16,
        16,
    );
    let system = f.surface(
        dd,
        true,
        creation::OFFSCREEN_PLAIN | creation::SYSTEM_MEMORY,
        16,
        16,
    );
    let device = f.device(d3d, Face::Direct3D7, primary);
    let set = |f: &mut Fixture, w, h, bits| {
        f.call("IDirectDraw7::SetDisplayMode", &[dd, w, h, bits, 0, 0])
    };
    let lost = |f: &mut Fixture, p| f.call("IDirectDrawSurface7::IsLost", &[p]);
    let desc = DATA + 0x100;

    assert_eq!(set(&mut f, 640, 480, 16), 0);
    assert_eq!(lost(&mut f, primary), 0x887601c2);
    assert_eq!(lost(&mut f, video), 0x887601c2);
    assert_eq!(lost(&mut f, system), 0);
    f.lock(primary, 0x10);
    f.unlock(primary);
    f.write(desc, &124u32.to_le_bytes());
    assert_eq!(
        f.call("IDirectDrawSurface7::Lock", &[video, 0, desc, 0x10, 0]),
        0x887601c2
    );
    assert_eq!(
        f.call("IDirectDrawSurface7::Blt", &[primary, 0, system, 0, 0, 0]),
        0x887601c2
    );
    assert_eq!(
        f.call("IDirectDraw7::RestoreAllSurfaces", &[dd]),
        0x8876024b
    );
    assert_eq!(f.device_call(device, "BeginScene", &[]), 0x887602fa);
    assert_eq!(f.call("IDirectDraw7::RestoreDisplayMode", &[dd]), 0);
    assert_eq!(f.call("IDirectDraw7::RestoreAllSurfaces", &[dd]), 0);
    assert_eq!(lost(&mut f, primary), 0);

    f.machine.ext.get_mut::<Active>().0 = false;
    assert_eq!(
        f.call("IDirectDraw7::TestCooperativeLevel", &[dd]),
        0x887600e1
    );
    assert_eq!(f.device_call(device, "BeginScene", &[]), 0x887601c2);
    f.machine.ext.get_mut::<Active>().0 = true;
    assert_eq!(f.call("IDirectDraw7::TestCooperativeLevel", &[dd]), 0);
    assert_eq!(f.device_call(device, "BeginScene", &[]), 0x887601c2);
    assert_eq!(f.call("IDirectDraw7::RestoreAllSurfaces", &[dd]), 0);
    assert_eq!(f.device_call(device, "BeginScene", &[]), 0);
    assert_eq!(f.device_call(device, "EndScene", &[]), 0);

    assert_eq!(set(&mut f, 640, 480, 8), 0);
    f.write(desc, &124u32.to_le_bytes());
    assert_eq!(f.call("IDirectDraw7::GetDisplayMode", &[dd, desc]), 0);
    assert_eq!((f.word(desc + 76), f.word(desc + 84)), (0x60, 8));
    assert_eq!(f.call("IDirectDrawSurface7::Release", &[primary]), 1);
    assert_eq!(f.call("IDirect3DDevice7::Release", &[device]), 0);
    let indexed = f.surface(dd, true, creation::PRIMARY, 640, 480);
    let entries = DATA + 0x400;
    let colors = [[32u8, 64, 128, 0]; 256];
    f.write(entries, bytemuck::cast_slice(&colors));
    let palette = f.output("IDirectDraw7::CreatePalette", &[dd, 0x44, entries, OUT, 0]);
    assert_eq!(
        f.call("IDirectDrawSurface7::SetPalette", &[indexed, palette]),
        0
    );
    assert_eq!(
        f.output("IDirectDrawSurface7::GetPalette", &[indexed, OUT]),
        palette
    );
    assert_eq!(f.call("IDirectDrawPalette::Release", &[palette]), 2);
    assert_eq!(f.call("IDirectDrawPalette::Release", &[palette]), 1);
    assert_eq!(f.call("IDirectDrawSurface7::Release", &[indexed]), 0);
    assert_eq!(f.call("IDirectDraw7::GetDisplayMode", &[dd, desc]), 0);
    assert_eq!(set(&mut f, 640, 480, 24), 0);
    assert_eq!(f.call("IDirectDraw7::GetDisplayMode", &[dd, desc]), 0);
    assert_eq!((f.word(desc + 76), f.word(desc + 84)), (0x40, 32));
    assert_eq!(set(&mut f, 700, 500, 32), 0x80004001);
    assert_eq!(f.call("IDirectDraw7::SetCooperativeLevel", &[dd, 1, 8]), 0);
    assert_eq!(
        f.call("IDirectDraw7::TestCooperativeLevel", &[dd]),
        0x8876024b
    );
}

// spec: GFX-D3D7-2, GFX-CAP-6
#[test]
fn legacy_caps_and_texture_callbacks_use_the_interface_layout() {
    use adv_abi::types::{D3DDEVICEDESC, DDPIXELFORMAT};
    let mut f = Fixture::new(true);
    let callback = DATA + 0xf00;
    f.machine.cpu.protect(DATA, 4096, Prot::RWX).unwrap();
    let dd = f.dd(1);
    let target = f.surface(
        dd,
        false,
        creation::OFFSCREEN_PLAIN | creation::THREE_D_DEVICE,
        32,
        32,
    );
    let device1 = f.face(target, IID_IDIRECT3DHALDEVICE);
    let d3d2 = f.face(dd, IID_IDIRECT3D2);
    let d3d3 = f.face(dd, IID_IDIRECT3D3);
    let device2 = f.device(d3d2, Face::Direct3D2, target);
    let target4 = f.face(target, IID_SURFACE4);
    let device3 = f.device(d3d3, Face::Direct3D3, target4);
    let mut formats = Vec::new();
    for (interface, pointer, desc) in [
        ("IDirect3DDevice", device1, true),
        ("IDirect3DDevice2", device2, true),
        ("IDirect3DDevice3", device3, false),
    ] {
        assert_eq!(
            f.invoke(
                &format!("{interface}::EnumTextureFormats"),
                &[pointer, callback, DATA],
                |cpu, args| {
                    assert_eq!(args[1], DATA);
                    let pixelformat = if desc {
                        assert_eq!(cpu.read_u32(args[0]).unwrap(), 108);
                        args[0] + 72
                    } else {
                        args[0]
                    };
                    let format: DDPIXELFORMAT =
                        adv_abi::GuestPtr::new(pixelformat).read(cpu).unwrap();
                    assert_eq!(format.dwSize, 32);
                    formats.push((format.dwFlags, format.dwRGBBitCount));
                    0
                }
            ),
            0
        );
        let address = DATA + 0x300;
        let size = size_of::<D3DDEVICEDESC>() as u32;
        f.write(address, &size.to_le_bytes());
        assert_eq!(
            f.call(&format!("{interface}::GetCaps"), &[pointer, address, 0]),
            0
        );
        let report: D3DDEVICEDESC = adv_abi::GuestPtr::new(address)
            .read(&f.machine.cpu)
            .unwrap();
        assert_eq!(report.dwMaxTextureWidth, 16384);
        assert_eq!(report.dwDevCaps & 0x30, 0x10);
    }
    assert_eq!(formats, vec![formats[0]; 3]);
}
