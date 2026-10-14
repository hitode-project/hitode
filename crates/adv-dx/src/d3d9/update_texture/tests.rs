//! Small authored 2D mip updates; all assertions concern guest state or pixels.
use super::*;
use adv_abi::types::D3DLOCKED_RECT;
use adv_cpu::{
    JitCpu,
    backend::{Guest, Reg},
};
use adv_device::gpu::Image;
use adv_machine::{Resume, WakeReason};

mod benchmark;

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
struct Fixture {
    m: Machine<JitCpu>,
    registry: Registry,
    device: u32,
}
impl Fixture {
    fn new() -> Self {
        let mut m = super::super::tests::machine();
        m.sys.objects = Objects::default();
        let com = Com::new(&mut m.sys.objects).unwrap();
        m.ext.insert(Surfaces::default());
        let state = m.ext.get_mut::<State>();
        state.com = com;
        state.tables.insert("IDirect3DTexture9", 0);
        state.gpu = Some(pollster::block_on(Gpu::request(!cfg!(target_os = "macos"))).unwrap());
        state.provider.insert = |ext, target| {
            let surfaces = ext.get_mut::<Surfaces>();
            surfaces.next += 1;
            surfaces.targets.insert(surfaces.next, target);
            Ok(surfaces.next)
        };
        state.provider.target = |ext, id| ext.get::<Surfaces>().targets.get(&id).cloned();
        state.provider.remove = |ext, _, id| {
            if id != 0 {
                assert!(ext.get_mut::<Surfaces>().targets.remove(&id).is_some());
            }
            Ok(())
        };
        // Admission-only assembly fixture: no GDI operation is performed on
        // this handle. Real canvas writeback is covered by the DC module.
        state.provider.acquire_dc = |_, _, _, _, _| Ok(0x1910);
        state.provider.writeback_dc = |_, _, _, _| Ok(());
        m.cpu.map(STACK, 4096, Prot::RW).unwrap();
        m.cpu.map(0x400000, 4096, Prot::RX).unwrap();
        m.spawn("texture updates", 0x400000, &[], 65536).unwrap();
        let (device, _, _, original) = super::super::tests::setup(&mut env(&mut m));
        let dead = com
            .release_internal(&mut m.sys.objects, original)
            .unwrap()
            .unwrap();
        destroy(&mut env(&mut m), dead);
        let mut registry = Registry::default();
        super::super::register(&mut registry);
        Self {
            m,
            registry,
            device,
        }
    }
    fn packet<T: bytemuck::Pod>(&mut self, value: T) -> u32 {
        let mut e = env(&mut self.m);
        let address = alloc(&mut e, std::mem::size_of::<T>() as u32).unwrap();
        GuestPtr::new(address).write(e.cpu, value).unwrap();
        address
    }
    fn call(&mut self, name: &str, args: &[u32]) -> Outcome {
        self.m.cpu.set_reg(Reg::Esp, STACK);
        for (i, &arg) in args.iter().enumerate() {
            self.m.cpu.write_u32(STACK + 4 + i as u32 * 4, arg).unwrap();
        }
        (self.registry.get("d3d9.dll", name).unwrap().handler)(&mut env(&mut self.m))
    }
    fn finish(&mut self, mut outcome: Outcome) -> u32 {
        loop {
            outcome = match outcome {
                Outcome::Return(value) => return value,
                Outcome::Block { then, .. } => {
                    super::super::pump(&mut self.m).unwrap();
                    then(&mut env(&mut self.m), Resume::Woke(WakeReason::Signaled))
                }
                _ => panic!("unexpected update outcome"),
            };
        }
    }
    fn invoke(&mut self, name: &str, args: &[u32]) -> u32 {
        let outcome = self.call(name, args);
        self.finish(outcome)
    }
    fn texture(&mut self, width: u32, height: u32, levels: u32, pool: u32, format: u32) -> u32 {
        let output = self.packet(0u32);
        let usage = if pool == 0 { 0x200 } else { 0 };
        assert_eq!(
            self.invoke(
                "IDirect3DDevice9::CreateTexture",
                &[
                    self.device,
                    width,
                    height,
                    levels,
                    usage,
                    format,
                    pool,
                    output,
                    0
                ]
            ),
            0
        );
        self.m.cpu.read_u32(output).unwrap()
    }
    fn lock(&mut self, texture: u32, level: u32, flags: u32) -> D3DLOCKED_RECT {
        let output = self.packet(D3DLOCKED_RECT::default());
        assert_eq!(
            self.invoke(
                "IDirect3DTexture9::LockRect",
                &[texture, level, output, 0, flags]
            ),
            0
        );
        GuestPtr::new(output).read(&self.m.cpu).unwrap()
    }
    fn fill(&mut self, texture: u32, level: u32, pixel: &[u8], flags: u32) {
        let locked = self.lock(texture, level, flags);
        let height = resources::resource(&env(&mut self.m), texture)
            .unwrap()
            .levels[level as usize]
            .desc
            .Height;
        self.m
            .cpu
            .write(
                locked.pBits,
                &pixel.repeat(locked.Pitch as usize * height as usize / pixel.len()),
            )
            .unwrap();
        assert_eq!(
            self.invoke("IDirect3DTexture9::UnlockRect", &[texture, level]),
            0
        );
    }
    fn image(&mut self, texture: u32, level: usize) -> Image {
        let e = env(&mut self.m);
        let surface = resources::resource(&e, texture).unwrap().levels[level]
            .surface
            .unwrap();
        e.ext.get::<Surfaces>().targets[&surface].image.clone()
    }
    fn pixels(&mut self, image: &Image) -> Vec<u8> {
        let mut readback = self
            .m
            .ext
            .get_mut::<State>()
            .gpu
            .as_mut()
            .unwrap()
            .readback(image, None)
            .unwrap();
        readback.take_completion_job().unwrap()();
        readback.poll().unwrap().unwrap()
    }
    fn dirty(&mut self, texture: u32) -> Vec<bool> {
        resources::resource(&env(&mut self.m), texture)
            .unwrap()
            .levels
            .iter()
            .map(|l| l.dirty)
            .collect()
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
    fn update(&mut self, source: u32, destination: u32) -> u32 {
        self.invoke(
            "IDirect3DDevice9::UpdateTexture",
            &[self.device, source, destination],
        )
    }
}

#[test]
// spec: DEV-UPDATE-1
fn tail_mips_upload_to_gpu_and_dynamic_shadow_and_clean_source_is_a_noop() {
    let mut f = Fixture::new();
    let source = f.texture(8, 4, 4, 2, 22);
    let destination = f.texture(4, 2, 3, 0, 22);
    for level in 0..4 {
        f.fill(source, level, &[10 + level as u8, 30, 50, 70], 0x8000);
    }
    assert_eq!(f.update(source, destination), 0);
    assert_eq!(f.dirty(source), [false; 4]);
    for level in 0..3 {
        let image = f.image(destination, level);
        assert_eq!(
            f.pixels(&image),
            [11 + level as u8, 30, 50, 255].repeat((4 >> level) * (2 >> level).max(1))
        );
        let locked = f.lock(destination, level as u32, 0x10);
        assert_eq!(
            f.m.cpu.read_u32(locked.pBits).unwrap().to_le_bytes(),
            [11 + level as u8, 30, 50, 70],
            "dynamic shadow retains source SDK bytes"
        );
        assert_eq!(
            f.invoke(
                "IDirect3DTexture9::UnlockRect",
                &[destination, level as u32]
            ),
            0
        );
    }
    f.fill(source, 1, &[7, 8, 9, 10], 0x8000);
    let before = f.submissions();
    assert_eq!(f.update(source, destination), 0);
    assert_eq!(f.submissions(), before);
    let image = f.image(destination, 0);
    assert_eq!(f.pixels(&image), [11, 30, 50, 255].repeat(8));
    assert_eq!(f.invoke("IDirect3DTexture9::AddDirtyRect", &[source, 0]), 0);
    assert_eq!(f.update(source, destination), 0);
    assert_eq!(f.pixels(&image), [7, 8, 9, 255].repeat(8));
    assert_eq!(f.dirty(source), [false; 4]);
    // Packed SDK bytes use the same upload conversion and keep a 2-byte shadow.
    let source = f.texture(2, 2, 1, 2, 26);
    let destination = f.texture(2, 2, 1, 0, 26);
    f.fill(source, 0, &[0x23, 0x81], 0);
    assert_eq!(f.update(source, destination), 0);
    let image = f.image(destination, 0);
    assert_eq!(f.pixels(&image), [51, 34, 17, 136].repeat(4));
    let locked = f.lock(destination, 0, 0x10);
    assert_eq!(locked.Pitch, 4);
    assert_eq!(f.m.cpu.read_u16(locked.pBits).unwrap(), 0x8123);
}

#[test]
// spec: DEV-UPDATE-1
fn invalid_pool_format_mip_shape_and_locks_or_dc_preserve_source_marks() {
    let mut f = Fixture::new();
    let source = f.texture(8, 4, 4, 2, 22);
    let destination = f.texture(4, 2, 3, 0, 22);
    let bad_source = f.texture(4, 2, 2, 2, 22);
    let managed = f.texture(4, 2, 3, 1, 22);
    let bad_shape = f.texture(4, 4, 3, 0, 22);
    let bad_format = f.texture(4, 2, 3, 0, 21);
    for (source, destination) in [
        (source, bad_shape),
        (source, bad_format),
        (bad_source, destination),
        (managed, destination),
        (source, managed),
    ] {
        let dirty = f.dirty(source);
        let before = f.submissions();
        assert_eq!(f.update(source, destination), INVALID);
        assert_eq!(f.dirty(source), dirty);
        assert_eq!(f.submissions(), before);
    }
    let locked_output = f.packet(D3DLOCKED_RECT::default());
    for (texture, level) in [(source, 2), (destination, 1)] {
        f.lock(texture, level, 0x10);
        let dirty = f.dirty(source);
        let before = f.submissions();
        assert_eq!(f.update(source, destination), INVALID);
        assert_eq!(f.dirty(source), dirty);
        assert_eq!(f.submissions(), before);
        assert_eq!(
            f.invoke("IDirect3DTexture9::UnlockRect", &[texture, level]),
            0
        );
    }
    let output = f.packet(0u32);
    assert_eq!(
        f.invoke("IDirect3DTexture9::GetSurfaceLevel", &[source, 0, output]),
        0
    );
    let surface = f.m.cpu.read_u32(output).unwrap();
    assert_eq!(f.invoke("IDirect3DSurface9::GetDC", &[surface, output]), 0);
    let dc = f.m.cpu.read_u32(output).unwrap();
    assert_eq!(f.update(source, destination), INVALID);
    assert_eq!(f.invoke("IDirect3DSurface9::ReleaseDC", &[surface, dc]), 0);
    assert_eq!(f.invoke("IDirect3DSurface9::Release", &[surface]), 1);
    assert_eq!(
        f.invoke(
            "IDirect3DTexture9::LockRect",
            &[source, 0, locked_output, 0, 0x10]
        ),
        0
    );
}

#[test]
// spec: DEV-UPDATE-1
fn inaccessible_staging_and_late_shadow_failure_restore_marks_for_retry() {
    let mut f = Fixture::new();
    let source = f.texture(4, 4, 3, 2, 21);
    let destination = f.texture(4, 4, 3, 0, 21);
    f.fill(source, 0, &[1, 2, 3, 4], 0);
    let source_page = resources::resource(&env(&mut f.m), source).unwrap().storage & !4095;
    let destination_page = resources::resource(&env(&mut f.m), destination)
        .unwrap()
        .storage
        & !4095;
    let before = f.submissions();
    for (page, protection) in [(source_page, Prot::WRITE), (destination_page, Prot::READ)] {
        f.m.cpu.protect(page, 4096, protection).unwrap();
        assert_eq!(f.update(source, destination), INVALID);
        assert_eq!(f.submissions(), before);
        assert_eq!(f.dirty(source), [true; 3]);
        f.m.cpu.protect(page, 4096, Prot::RW).unwrap();
    }
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
        "IDirect3DDevice9::UpdateTexture",
        &[f.device, source, destination],
    );
    assert!(matches!(outcome, Outcome::Block { .. }));
    f.m.cpu.protect(destination_page, 4096, Prot::READ).unwrap();
    drop(pause);
    assert_eq!(f.finish(outcome), INVALID);
    assert_eq!(f.dirty(source), [true; 3]);
    assert!(f.m.ext.get::<Transfers>().active.is_empty());
    assert!(
        resources::resource(&env(&mut f.m), destination)
            .unwrap()
            .levels
            .iter()
            .all(|l| l.lock.is_none())
    );
    f.m.cpu.protect(destination_page, 4096, Prot::RW).unwrap();
    assert_eq!(f.update(source, destination), 0);
    let image = f.image(destination, 0);
    assert_eq!(f.pixels(&image), [1, 2, 3, 4].repeat(16));
    assert_eq!(f.dirty(source), [false; 3]);
}

#[test]
// spec: DEV-UPDATE-1
fn busy_updates_keep_snapshot_new_dirty_marks_and_com_owners_until_cancel() {
    let mut f = Fixture::new();
    let source = f.texture(4, 4, 3, 2, 22);
    let destination = f.texture(4, 4, 3, 0, 22);
    f.fill(source, 0, &[1, 2, 3, 4], 0);
    let image = f.image(destination, 0);
    let source_storage = resources::resource(&env(&mut f.m), source).unwrap().storage;
    let destination_storage = resources::resource(&env(&mut f.m), destination)
        .unwrap()
        .storage;
    let output = f.packet(D3DLOCKED_RECT::default());
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
        "IDirect3DDevice9::UpdateTexture",
        &[f.device, source, destination],
    );
    assert!(matches!(outcome, Outcome::Block { .. }));
    assert_eq!(
        f.invoke("IDirect3DTexture9::LockRect", &[source, 1, output, 0, 0]),
        INVALID
    );
    f.m.cpu
        .write(source_storage, &[9, 8, 7, 6].repeat(16))
        .unwrap();
    assert_eq!(f.invoke("IDirect3DTexture9::AddDirtyRect", &[source, 0]), 0);
    drop(pause);
    assert!(super::super::pump(&mut f.m).unwrap());
    assert!(
        resources::resource(&env(&mut f.m), destination)
            .unwrap()
            .levels[0]
            .lock
            .is_none()
    );
    assert_eq!(
        f.invoke("IDirect3DTexture9::UnlockRect", &[destination, 0]),
        INVALID,
        "reservation survives the first upload completion"
    );
    assert_eq!(f.finish(outcome), 0);
    assert_eq!(f.pixels(&image), [1, 2, 3, 255].repeat(16));
    assert_eq!(
        f.dirty(source),
        [true; 3],
        "new dirty marks survive snapshot completion"
    );
    assert_eq!(f.update(source, destination), 0);
    assert_eq!(f.pixels(&image), [9, 8, 7, 255].repeat(16));
    assert_eq!(f.invoke("IDirect3DTexture9::AddDirtyRect", &[source, 0]), 0);
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
        "IDirect3DDevice9::UpdateTexture",
        &[f.device, source, destination],
    );
    assert!(matches!(outcome, Outcome::Block { .. }));
    assert_eq!(f.invoke("IDirect3DTexture9::Release", &[source]), 0);
    assert_eq!(f.invoke("IDirect3DTexture9::Release", &[destination]), 0);
    assert_eq!(f.invoke("IDirect3DDevice9::Release", &[f.device]), 0);
    assert!(
        f.m.ext
            .get::<Heaps>()
            .owner(&f.m.cpu, source_storage)
            .is_some()
    );
    assert!(
        f.m.ext
            .get::<Heaps>()
            .owner(&f.m.cpu, destination_storage)
            .is_some()
    );
    super::super::cancel_pending(&mut f.m);
    drop(outcome);
    drop(pause);
    assert!(f.m.ext.get::<Transfers>().active.is_empty());
    assert!(f.m.ext.get::<State>().pending.is_empty());
    assert!(f.m.ext.get::<Surfaces>().targets.is_empty());
    assert!(
        f.m.ext
            .get::<Heaps>()
            .owner(&f.m.cpu, source_storage)
            .is_none()
    );
    assert!(
        f.m.ext
            .get::<Heaps>()
            .owner(&f.m.cpu, destination_storage)
            .is_none()
    );
}

#[test]
// spec: DEV-UPDATE-2
fn bgra_snapshot_survives_busy_upload_and_later_dirty_rects() {
    let mut f = Fixture::new();
    let source = f.texture(8, 4, 1, 2, 21);
    let destination = f.texture(8, 4, 1, 0, 21);
    f.fill(source, 0, &[1, 2, 3, 4], 0);
    let image = f.image(destination, 0);
    let storage = resources::resource(&env(&mut f.m), source).unwrap().storage;
    let rectangle = f.packet(RECT {
        left: 1,
        top: 1,
        right: 3,
        bottom: 2,
    });
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
        "IDirect3DDevice9::UpdateTexture",
        &[f.device, source, destination],
    );
    assert!(matches!(outcome, Outcome::Block { .. }));
    f.m.cpu.write(storage, &[9, 8, 7, 6].repeat(32)).unwrap();
    assert_eq!(
        f.invoke("IDirect3DTexture9::AddDirtyRect", &[source, rectangle]),
        0
    );
    drop(pause);
    assert_eq!(f.finish(outcome), 0);
    assert_eq!(f.pixels(&image), [1, 2, 3, 4].repeat(32));
    let locked = f.lock(destination, 0, 0x10);
    assert_eq!(
        f.m.cpu.read_u32(locked.pBits).unwrap().to_le_bytes(),
        [1, 2, 3, 4]
    );
    assert_eq!(
        f.invoke("IDirect3DTexture9::UnlockRect", &[destination, 0]),
        0
    );
    assert_eq!(f.dirty(source), [true]);
    assert_eq!(f.update(source, destination), 0);
    assert_eq!(f.pixels(&image), [9, 8, 7, 6].repeat(32));
    assert_eq!(f.dirty(source), [false]);
}

#[test]
// spec: DEV-UPDATE-4
fn partial_dirty_marks_copy_matching_tail_mips_and_no_dirty_lock_stays_clean() {
    let mut f = Fixture::new();
    let source = f.texture(16, 8, 5, 2, 21);
    let destination = f.texture(8, 4, 4, 0, 21);
    for level in 0..5 {
        f.fill(source, level, &[level as u8, 2, 3, 4], 0x8000);
    }
    assert_eq!(f.update(source, destination), 0);
    let image = f.image(destination, 1);
    assert_eq!(f.pixels(&image), [2, 2, 3, 4].repeat(8));
    f.fill(source, 2, &[11, 12, 13, 14], 0x8000);
    assert_eq!(f.update(source, destination), 0);
    assert_eq!(f.pixels(&image), [2, 2, 3, 4].repeat(8));
    let first = f.packet(RECT {
        left: 1,
        top: 1,
        right: 4,
        bottom: 3,
    });
    let second = f.packet(RECT {
        left: 3,
        top: 2,
        right: 7,
        bottom: 5,
    });
    for rectangle in [first, second] {
        assert_eq!(
            f.invoke("IDirect3DTexture9::AddDirtyRect", &[source, rectangle]),
            0
        );
    }
    assert_eq!(f.update(source, destination), 0);
    assert_eq!(f.pixels(&image), [11, 12, 13, 14].repeat(8));
    assert_eq!(f.dirty(source), [false; 5]);
    let output = f.packet(D3DLOCKED_RECT::default());
    assert_eq!(
        f.invoke(
            "IDirect3DTexture9::LockRect",
            &[source, 0, output, first, 0]
        ),
        0
    );
    assert_eq!(f.invoke("IDirect3DTexture9::UnlockRect", &[source, 0]), 0);
    assert_eq!(f.dirty(source), [true; 5]);
    assert_eq!(f.update(source, destination), 0);
    assert_eq!(f.dirty(source), [false; 5]);
}

#[test]
fn updated_render_target_texture_is_visible_to_get_render_target_data() {
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };
    let mut f = Fixture::new();
    let wake = Arc::new(adv_sync::WakeEvent::new().unwrap());
    let mut builder = adv_sync::background::Builder::new((*wake).clone()).unwrap();
    let sender = builder.sender(8, 8).unwrap();
    let _service = builder.start().unwrap();
    let state = f.m.ext.get_mut::<State>();
    state.sender = Some(std::rc::Rc::new(std::cell::RefCell::new(sender)));
    state.wake = Some(wake.clone());
    let source = f.texture(8, 4, 1, 2, 21);
    f.fill(source, 0, &[31, 41, 59, 26], 0);
    let output = f.packet(0u32);
    assert_eq!(
        f.invoke(
            "IDirect3DDevice9::CreateTexture",
            &[f.device, 8, 4, 1, 1, 21, 0, output, 0]
        ),
        0
    );
    let destination = f.m.cpu.read_u32(output).unwrap();
    assert_eq!(f.update(source, destination), 0);
    assert_eq!(
        f.invoke(
            "IDirect3DTexture9::GetSurfaceLevel",
            &[destination, 0, output]
        ),
        0
    );
    let surface = f.m.cpu.read_u32(output).unwrap();
    assert_eq!(
        f.invoke(
            "IDirect3DDevice9::CreateOffscreenPlainSurface",
            &[f.device, 8, 4, 21, 2, output, 0]
        ),
        0
    );
    let staging = f.m.cpu.read_u32(output).unwrap();
    let outcome = f.call(
        "IDirect3DDevice9::GetRenderTargetData",
        &[f.device, surface, staging],
    );
    let Outcome::Block {
        wait: Wait::Object { handle, .. },
        then,
        ..
    } = outcome
    else {
        panic!("expected readback continuation")
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        super::super::pump(&mut f.m).unwrap();
        if matches!(
            f.m.sys.objects.get(handle),
            Some(Object::Event { signaled: true, .. })
        ) {
            break;
        }
        assert!(Instant::now() < deadline, "bounded readback wait");
        wake.wait(Some(Duration::from_millis(10))).unwrap();
    }
    assert!(matches!(
        then(&mut env(&mut f.m), Resume::Woke(WakeReason::Signaled)),
        Outcome::Return(0)
    ));
    let locked = f.packet(D3DLOCKED_RECT::default());
    assert_eq!(
        f.invoke("IDirect3DSurface9::LockRect", &[staging, locked, 0, 0x10]),
        0
    );
    let locked: D3DLOCKED_RECT = GuestPtr::new(locked).read(&f.m.cpu).unwrap();
    let mut pixels = vec![0; 8 * 4 * 4];
    f.m.cpu.read(locked.pBits, &mut pixels).unwrap();
    assert_eq!(pixels, [31, 41, 59, 26].repeat(32));
}

#[test]
// spec: DEV-UPDATE-2
fn successive_sizes_formats_and_mip_chains_keep_original_shadow_and_gpu_pixels() {
    let mut f = Fixture::new();
    for (width, height, levels, format) in
        [(16, 8, 1, 22), (4, 2, 3, 26), (2, 2, 1, 21), (16, 8, 5, 22)]
    {
        let source = f.texture(width, height, levels, 2, format);
        let destination = f.texture(width, height, levels, 0, format);
        let (raw, bgra) = match format {
            26 => (vec![0x23, 0x81], vec![51, 34, 17, 136]),
            22 => (vec![1, 2, 3, 4], vec![1, 2, 3, 255]),
            _ => (vec![5, 6, 7, 8], vec![5, 6, 7, 8]),
        };
        for level in 0..levels {
            f.fill(source, level, &raw, 0x8000);
        }
        assert_eq!(f.update(source, destination), 0);
        for level in 0..levels {
            let count = ((width >> level).max(1) * (height >> level).max(1)) as usize;
            let image = f.image(destination, level as usize);
            assert_eq!(f.pixels(&image), bgra.repeat(count));
            let locked = f.lock(destination, level, 0x10);
            let mut shadow = vec![0; raw.len() * count];
            f.m.cpu.read(locked.pBits, &mut shadow).unwrap();
            assert_eq!(shadow, raw.repeat(count));
            assert_eq!(
                f.invoke("IDirect3DTexture9::UnlockRect", &[destination, level]),
                0
            );
        }
        assert_eq!(f.invoke("IDirect3DTexture9::Release", &[source]), 0);
        assert_eq!(f.invoke("IDirect3DTexture9::Release", &[destination]), 0);
    }
}
