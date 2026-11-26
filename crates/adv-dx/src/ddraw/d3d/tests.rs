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
        match (f.handler)(&mut env) {
            Outcome::Return(hr) => hr,
            Outcome::Fail(error) => panic!("{name}: {error}"),
            _ => panic!("{name}: unexpected continuation"),
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

    fn device(&mut self, d3d: u32, face: Face, surface: u32) -> u32 {
        self.write(DATA, &IID_IDIRECT3DHALDEVICE);
        let mut args = vec![d3d, DATA, surface, OUT];
        if face == Face::Direct3D3 {
            args.push(0);
        }
        self.output(&format!("{}::CreateDevice", face.name()), &args)
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
    let mut f = Fixture::new(false);
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
