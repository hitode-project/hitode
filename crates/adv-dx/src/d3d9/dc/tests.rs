//! Authored GDI/D3D interoperation, including real asynchronous GPU readback.
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Guest, Reg},
};
use adv_device::surface::{MemoryOperations, MemoryView, Snapshot, SurfaceStore};
use adv_machine::{Resume, WakeReason};
use adv_win32::{Win32, Win32Config};
use std::{
    borrow::Cow,
    sync::Arc,
    time::{Duration, Instant},
};

const STACK: u32 = 0x550000;
fn env(m: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    }
}
fn memory<'a>(cpu: &'a mut (dyn Guest + 'a)) -> MemoryView<'a, dyn Guest + 'a> {
    MemoryView::new(
        cpu,
        MemoryOperations {
            read: |cpu, address, length| {
                let mut bytes = vec![0; length];
                cpu.read(address, &mut bytes).map_err(|e| e.to_string())?;
                Ok(Cow::Owned(bytes))
            },
            write: |cpu, address, bytes| cpu.write(address, bytes).map_err(|e| e.to_string()),
            writable: |cpu, address, length| {
                if cpu.accessible(address, length as u32, Prot::WRITE) {
                    Ok(())
                } else {
                    Err("not writable".into())
                }
            },
            copy: |cpu, source, destination, length| {
                cpu.copy_memory(source, destination, length)
                    .map_err(|e| e.to_string())
            },
        },
    )
}
struct Fixture {
    m: Machine<JitCpu>,
    registry: Registry,
    device: u32,
    wake: Arc<adv_sync::WakeEvent>,
    _service: adv_sync::background::Service,
}
impl Fixture {
    fn new(gpu: bool) -> Self {
        let mut m = super::super::tests::machine();
        m.sys.objects = Objects::default();
        Win32::install(
            &mut m,
            Win32Config::new(".", "surface-dc.exe", "target/surface-dc"),
        )
        .unwrap();
        let arena = m.sys.vm.alloc(&mut m.cpu, 1024 * 1024, Prot::RW).unwrap();
        m.ext.insert(
            SurfaceStore::new(
                4,
                4,
                arena,
                1024 * 1024,
                Snapshot {
                    width: 4,
                    height: 4,
                    bgra: vec![0; 64],
                    frames: 0,
                    ..Default::default()
                },
            )
            .unwrap(),
        );
        adv_win32::gdi32::raster::set_provider(
            &mut m.ext,
            adv_win32::gdi32::raster::DrawingProvider {
                memory,
                completed: |_, _| None,
            },
        );
        let com = Com::new(&mut m.sys.objects).unwrap();
        let state = m.ext.get_mut::<State>();
        state.com = com;
        state.tables.insert("IDirect3DTexture9", 0);
        state.provider.acquire_dc = adv_win32::gdi32::raster::front::surface_dc::acquire;
        state.provider.writeback_dc = adv_win32::gdi32::raster::front::surface_dc::writeback;
        state.provider.release_dc = adv_win32::gdi32::raster::front::surface_dc::release;
        state.provider.insert = |ext, target| ext.get_mut::<SurfaceStore>().insert_native(target);
        state.provider.target = |ext, id| ext.get::<SurfaceStore>().native_target(id).cloned();
        state.provider.remove = |ext, _, id| {
            if id != 0 {
                assert!(ext.get_mut::<SurfaceStore>().delete(id));
            }
            Ok(())
        };
        if gpu {
            state.gpu = Some(pollster::block_on(Gpu::request(!cfg!(target_os = "macos"))).unwrap());
        }
        let wake = Arc::new(adv_sync::WakeEvent::new().unwrap());
        let mut builder = adv_sync::background::Builder::new((*wake).clone()).unwrap();
        let sender = builder.sender(8, 8).unwrap();
        let service = builder.start().unwrap();
        state.sender = Some(std::rc::Rc::new(std::cell::RefCell::new(sender)));
        state.wake = Some(wake.clone());
        m.cpu.map(STACK, 4096, Prot::RW).unwrap();
        m.cpu.map(0x400000, 4096, Prot::RX).unwrap();
        m.spawn("surface DC fixture", 0x400000, &[], 65536).unwrap();
        let (device, _, _, original) = super::super::tests::setup(&mut env(&mut m));
        let dead = com
            .release_internal(&mut m.sys.objects, original)
            .unwrap()
            .unwrap();
        destroy(&mut env(&mut m), dead);
        let mut registry = Registry::default();
        super::super::register(&mut registry);
        adv_win32::register(&mut registry);
        Self {
            m,
            registry,
            device,
            wake,
            _service: service,
        }
    }
    fn packet<T: bytemuck::Pod>(&mut self, value: T) -> u32 {
        let mut e = env(&mut self.m);
        let address = alloc(&mut e, std::mem::size_of::<T>() as u32).unwrap();
        GuestPtr::new(address).write(e.cpu, value).unwrap();
        address
    }
    fn call(&mut self, module: &str, name: &str, args: &[u32]) -> Outcome {
        self.m.cpu.set_reg(Reg::Esp, STACK);
        for (i, &arg) in args.iter().enumerate() {
            self.m.cpu.write_u32(STACK + 4 + i as u32 * 4, arg).unwrap();
        }
        (self.registry.get(module, name).unwrap().handler)(&mut env(&mut self.m))
    }
    fn finish(&mut self, mut outcome: Outcome) -> u32 {
        loop {
            outcome = match outcome {
                Outcome::Return(result) => return result,
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
                        assert!(Instant::now() < deadline, "bounded GPU wait");
                        self.wake.wait(Some(Duration::from_millis(10))).unwrap();
                    }
                    then(&mut env(&mut self.m), Resume::Woke(WakeReason::Signaled))
                }
                _ => panic!("unexpected graphics outcome"),
            };
        }
    }
    fn d3d(&mut self, name: &str, args: &[u32]) -> u32 {
        let result = self.call("d3d9.dll", name, args);
        self.finish(result)
    }
    fn gdi(&mut self, name: &str, args: &[u32]) -> u32 {
        let result = self.call("gdi32.dll", name, args);
        self.finish(result)
    }
    fn surface(&mut self, pool: u32, format: u32) -> u32 {
        let output = self.packet(0u32);
        assert_eq!(
            self.d3d(
                "IDirect3DDevice9::CreateOffscreenPlainSurface",
                &[self.device, 4, 4, format, pool, output, 0]
            ),
            0
        );
        self.m.cpu.read_u32(output).unwrap()
    }
    fn texture(&mut self) -> (u32, u32, u32) {
        let output = self.packet(0u32);
        assert_eq!(
            self.d3d(
                "IDirect3DDevice9::CreateTexture",
                &[self.device, 4, 4, 3, 0, 22, 2, output, 0]
            ),
            0
        );
        let texture = self.m.cpu.read_u32(output).unwrap();
        assert_eq!(
            self.d3d("IDirect3DTexture9::GetSurfaceLevel", &[texture, 0, output]),
            0
        );
        let top = self.m.cpu.read_u32(output).unwrap();
        assert_eq!(
            self.d3d("IDirect3DTexture9::GetSurfaceLevel", &[texture, 1, output]),
            0
        );
        (texture, top, self.m.cpu.read_u32(output).unwrap())
    }
    fn acquire(&mut self, surface: u32) -> u32 {
        let output = self.packet(0u32);
        assert_eq!(self.d3d("IDirect3DSurface9::GetDC", &[surface, output]), 0);
        self.m.cpu.read_u32(output).unwrap()
    }
}

#[test]
fn gdi_dc_owns_mip_and_device_until_matching_release_and_blocks_public_unlock() {
    let mut f = Fixture::new(false);
    let (texture, top, sibling) = f.texture();
    let locked = f.packet(D3DLOCKED_RECT::default());
    let output = f.packet(0x12345678u32);
    assert_eq!(
        f.d3d("IDirect3DTexture9::LockRect", &[texture, 1, locked, 0, 0]),
        0
    );
    assert_eq!(
        f.d3d("IDirect3DSurface9::GetDC", &[top, output]),
        INVALID,
        "any locked mip excludes GetDC"
    );
    assert_eq!(f.m.cpu.read_u32(output).unwrap(), 0x12345678);
    assert_eq!(f.d3d("IDirect3DTexture9::UnlockRect", &[texture, 1]), 0);
    let dc = f.acquire(top);
    let bitmap = match f
        .m
        .ext
        .get::<Win32>()
        .gdi
        .get(&f.m.sys.objects, dc)
        .unwrap()
    {
        adv_os::gdi::GdiObj::Dc { selected, .. } => selected[3],
        _ => panic!(),
    };
    assert_eq!(f.gdi("DeleteDC", &[dc]), 0);
    assert_eq!(f.gdi("DeleteObject", &[bitmap]), 0);
    assert_eq!(f.gdi("SelectObject", &[dc, adv_os::gdi::STOCK_BITMAP]), 0);
    assert_eq!(
        f.d3d("IDirect3DSurface9::ReleaseDC", &[top, dc + 1]),
        INVALID
    );
    assert_eq!(
        f.d3d("IDirect3DSurface9::GetDC", &[sibling, output]),
        INVALID
    );
    assert_eq!(
        f.d3d("IDirect3DSurface9::LockRect", &[sibling, locked, 0, 0]),
        INVALID
    );
    assert_eq!(
        f.d3d("IDirect3DTexture9::UnlockRect", &[texture, 0]),
        INVALID
    );
    assert_eq!(f.d3d("IDirect3DSurface9::UnlockRect", &[top]), INVALID);
    assert_eq!(f.gdi("SetPixel", &[dc, 1, 2, 0x00332211]), 0x00332211);
    assert_eq!(f.gdi("GetPixel", &[dc, 1, 2]), 0x00332211);
    let storage = {
        let e = env(&mut f.m);
        resources::storage(&e, resources::resource(&e, top).unwrap()).unwrap()
    };
    let page = storage & !4095;
    f.m.cpu.protect(page, 4096, Prot::READ).unwrap();
    assert_eq!(f.d3d("IDirect3DSurface9::ReleaseDC", &[top, dc]), INVALID);
    assert_eq!(
        f.m.ext.get::<State>().dcs.len(),
        1,
        "writeback failure leaves the DC usable"
    );
    f.m.cpu.protect(page, 4096, Prot::RW).unwrap();
    assert_eq!(f.d3d("IDirect3DSurface9::ReleaseDC", &[top, dc]), 0);
    assert_eq!(
        f.m.cpu.read_u32(storage + (2 * 4 + 1) * 4).unwrap() & 0xffffff,
        0x112233
    );
    assert_eq!(f.m.ext.get::<SurfaceStore>().live_surfaces(), 0);
    assert_eq!(f.d3d("IDirect3DSurface9::ReleaseDC", &[top, dc]), INVALID);

    let dc = f.acquire(sibling);
    let lease = f.m.ext.get::<State>().dcs[0];
    assert_eq!(f.d3d("IDirect3DSurface9::Release", &[top]), 2);
    assert_eq!(f.d3d("IDirect3DSurface9::Release", &[sibling]), 1);
    assert_eq!(f.d3d("IDirect3DTexture9::Release", &[texture]), 0);
    assert_eq!(f.d3d("IDirect3DDevice9::Release", &[f.device]), 0);
    assert!(
        f.m.ext
            .get::<State>()
            .com
            .get_identity(&f.m.sys.objects, lease.owner)
            .is_some()
    );
    assert_eq!(f.gdi("SetPixel", &[dc, 0, 0, 0x00010203]), 0x00010203);
    assert_eq!(f.d3d("IDirect3DSurface9::ReleaseDC", &[sibling, dc]), 0);
    assert!(f.m.ext.get::<State>().dcs.is_empty());
    assert!(
        f.m.ext
            .get::<Heaps>()
            .owner(&f.m.cpu, lease.scratch)
            .is_none()
    );
    assert!(
        f.m.ext
            .get::<State>()
            .com
            .get_identity(&f.m.sys.objects, lease.owner)
            .is_none()
    );
    assert!(
        f.m.ext
            .get::<State>()
            .com
            .get_identity(&f.m.sys.objects, lease.device)
            .is_none()
    );
}

#[test]
fn invalid_output_alpha_and_scratch_admission_leave_no_dc_or_extra_owner() {
    let mut f = Fixture::new(false);
    let output = f.packet(0x12345678u32);
    for (pool, format) in [(2, 21), (3, 22)] {
        let surface = f.surface(pool, format);
        assert_eq!(
            f.d3d("IDirect3DSurface9::GetDC", &[surface, output]),
            INVALID
        );
        assert_eq!(f.m.cpu.read_u32(output).unwrap(), 0x12345678);
        assert_eq!(f.d3d("IDirect3DSurface9::Release", &[surface]), 0);
    }
    let surface = f.surface(2, 22);
    let objects = f.m.sys.objects.object_count();
    assert_eq!(f.d3d("IDirect3DSurface9::GetDC", &[surface, 0]), INVALID);
    assert_eq!(f.m.sys.objects.object_count(), objects);
    assert!(f.m.ext.get::<State>().dcs.is_empty());
    assert_eq!(f.m.ext.get::<SurfaceStore>().live_surfaces(), 0);
}

#[test]
fn default_surface_get_dc_waits_for_gpu_and_release_uploads_gdi_pixels() {
    let mut f = Fixture::new(true);
    let surface = f.surface(0, 22);
    let locked = f.packet(D3DLOCKED_RECT::default());
    assert_eq!(
        f.d3d("IDirect3DSurface9::LockRect", &[surface, locked, 0, 0]),
        0
    );
    let value: D3DLOCKED_RECT = GuestPtr::new(locked).read(&f.m.cpu).unwrap();
    let pattern: Vec<u8> = (0..16).flat_map(|i| [i, i + 1, i + 2, 0]).collect();
    f.m.cpu.write(value.pBits, &pattern).unwrap();
    assert_eq!(f.d3d("IDirect3DSurface9::UnlockRect", &[surface]), 0);
    f.m.cpu.write(value.pBits, &[0xee; 64]).unwrap();
    let output = f.packet(0x12345678u32);
    let outcome = f.call("d3d9.dll", "IDirect3DSurface9::GetDC", &[surface, output]);
    assert!(
        matches!(outcome, Outcome::Block { .. }),
        "GPU DC acquisition uses the readback worker"
    );
    assert_eq!(
        f.m.cpu.read_u32(output).unwrap(),
        0x12345678,
        "HDC is not published while pending"
    );
    assert_eq!(f.finish(outcome), 0);
    let dc = f.m.cpu.read_u32(output).unwrap();
    assert_eq!(
        f.gdi("GetPixel", &[dc, 0, 0]),
        0x00000102,
        "GPU contents replace stale staging"
    );
    assert_eq!(f.gdi("SetPixel", &[dc, 2, 1, 0x00554433]), 0x00554433);
    assert_eq!(f.d3d("IDirect3DSurface9::ReleaseDC", &[surface, dc]), 0);
    assert_eq!(
        f.m.ext.get::<SurfaceStore>().live_surfaces(),
        1,
        "only original GPU surface remains"
    );
    assert_eq!(
        f.d3d("IDirect3DSurface9::LockRect", &[surface, locked, 0, 0x10]),
        0
    );
    let value: D3DLOCKED_RECT = GuestPtr::new(locked).read(&f.m.cpu).unwrap();
    assert_eq!(
        f.m.cpu.read_u32(value.pBits + 6 * 4).unwrap() & 0xffffff,
        0x334455
    );
    assert_eq!(
        f.m.cpu.read_u32(value.pBits).unwrap() & 0xffffff,
        0x020100,
        "untouched GPU pixels survive GDI edits"
    );
    assert_eq!(f.d3d("IDirect3DSurface9::UnlockRect", &[surface]), 0);
    assert_eq!(f.d3d("IDirect3DSurface9::Release", &[surface]), 0);
    assert_eq!(f.m.ext.get::<SurfaceStore>().live_surfaces(), 0);
}

#[test]
fn dirty_marks_wait_for_release_dc_continuation_after_a_busy_upload() {
    let mut f = Fixture::new(true);
    let output = f.packet(0u32);
    assert_eq!(
        f.d3d(
            "IDirect3DDevice9::CreateTexture",
            &[f.device, 4, 4, 1, 0, 22, 1, output, 0]
        ),
        0
    );
    let texture = f.m.cpu.read_u32(output).unwrap();
    assert_eq!(
        f.d3d("IDirect3DTexture9::GetSurfaceLevel", &[texture, 0, output]),
        0
    );
    let surface = f.m.cpu.read_u32(output).unwrap();
    let dc = f.acquire(surface);
    assert_eq!(f.gdi("SetPixel", &[dc, 0, 0, 0x00010203]), 0x00010203);
    let pause =
        f.m.ext
            .get::<State>()
            .gpu
            .as_ref()
            .unwrap()
            .context()
            .pause_submissions()
            .unwrap();
    let outcome = f.call("d3d9.dll", "IDirect3DSurface9::ReleaseDC", &[surface, dc]);
    assert!(matches!(outcome, Outcome::Block { .. }));
    drop(pause);
    assert!(super::super::pump(&mut f.m).unwrap());
    assert!(
        resources::resource(&env(&mut f.m), texture).unwrap().levels[0]
            .lock
            .is_none(),
        "upload finished before its guest continuation"
    );
    let submissions =
        f.m.ext
            .get::<State>()
            .gpu
            .as_ref()
            .unwrap()
            .stats()
            .submissions;
    assert_eq!(f.d3d("IDirect3DTexture9::AddDirtyRect", &[texture, 0]), 0);
    assert_eq!(
        f.m.ext
            .get::<State>()
            .gpu
            .as_ref()
            .unwrap()
            .stats()
            .submissions,
        submissions,
        "live DC lease defers a competing dirty upload"
    );
    assert!(resources::resource(&env(&mut f.m), texture).unwrap().levels[0].dirty);
    assert_eq!(f.finish(outcome), 0);
    assert!(f.m.ext.get::<State>().dcs.is_empty());
    assert!(!resources::resource(&env(&mut f.m), texture).unwrap().levels[0].dirty);
    assert_eq!(f.d3d("IDirect3DSurface9::Release", &[surface]), 1);
    assert_eq!(f.d3d("IDirect3DTexture9::Release", &[texture]), 0);
    assert_eq!(f.m.ext.get::<SurfaceStore>().live_surfaces(), 0);
}

#[test]
fn pending_get_dc_output_failure_and_cancel_return_scratch_and_internal_refs() {
    let mut f = Fixture::new(true);
    let surface = f.surface(0, 22);
    let page = f.m.sys.vm.alloc(&mut f.m.cpu, 4096, Prot::RW).unwrap();
    f.m.cpu.write_u32(page, 0x12345678).unwrap();
    for cancel in [false, true] {
        let outcome = f.call("d3d9.dll", "IDirect3DSurface9::GetDC", &[surface, page]);
        assert!(matches!(outcome, Outcome::Block { .. }));
        let lease = f.m.ext.get::<State>().dcs[0];
        let owner_count = f.m.sys.objects.strong_count(lease.owner).unwrap();
        let device_count = f.m.sys.objects.strong_count(lease.device).unwrap();
        if cancel {
            super::super::cancel_pending(&mut f.m);
            drop(outcome);
        } else {
            f.m.cpu.protect(page, 4096, Prot::READ).unwrap();
            assert_eq!(f.finish(outcome), INVALID);
            f.m.cpu.protect(page, 4096, Prot::RW).unwrap();
        }
        assert_eq!(f.m.cpu.read_u32(page).unwrap(), 0x12345678);
        assert!(f.m.ext.get::<State>().dcs.is_empty());
        assert!(f.m.ext.get::<State>().transfers.is_empty());
        assert!(
            f.m.ext
                .get::<Heaps>()
                .owner(&f.m.cpu, lease.scratch)
                .is_none()
        );
        assert_eq!(
            f.m.sys.objects.strong_count(lease.owner),
            Some(owner_count - 2)
        );
        assert_eq!(
            f.m.sys.objects.strong_count(lease.device),
            Some(device_count - 2)
        );
        assert!(
            resources::resource(&env(&mut f.m), surface)
                .unwrap()
                .lock
                .is_none()
        );
        assert_eq!(f.m.ext.get::<SurfaceStore>().live_surfaces(), 1);
    }
}
