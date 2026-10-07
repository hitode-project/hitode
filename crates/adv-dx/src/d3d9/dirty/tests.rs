//! Authored managed/system-memory updates, no host window or game.
use super::*;
use adv_abi::{ImplementationLevel, types::D3DLOCKED_RECT};
use adv_cpu::{
    JitCpu,
    backend::{Guest, Reg},
};
use adv_device::gpu::Image;
use adv_machine::{Resume, WakeReason};

const STACK: u32 = 0x550000;
#[derive(Default)]
struct Surfaces {
    next: u32,
    targets: BTreeMap<u32, Target>,
}
fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn machine(gpu: bool) -> (Machine<JitCpu>, u32) {
    let mut m = super::super::tests::machine();
    m.cpu.map(STACK, 4096, Prot::RW).unwrap();
    m.cpu.map(0x400000, 4096, Prot::RX).unwrap();
    m.spawn("fixture", 0x400000, &[], 65536).unwrap();
    m.ext.insert(Surfaces::default());
    if gpu {
        m.ext.get_mut::<State>().gpu = Some(pollster::block_on(Gpu::request(true)).unwrap());
    }
    let state = m.ext.get_mut::<State>();
    state.tables.insert("IDirect3DTexture9", 0);
    state.provider.insert = |ext, target| {
        let surfaces = ext.get_mut::<Surfaces>();
        surfaces.next += 1;
        surfaces.targets.insert(surfaces.next, target);
        Ok(surfaces.next)
    };
    state.provider.target = |ext, surface| ext.get::<Surfaces>().targets.get(&surface).cloned();
    state.provider.remove = |ext, _, surface| {
        ext.get_mut::<Surfaces>().targets.remove(&surface).unwrap();
        Ok(())
    };
    let (device, _, _, _) = super::super::tests::setup(&mut env(&mut m));
    (m, device)
}
fn call(m: &mut Machine<JitCpu>, name: &str, args: &[u32]) -> Outcome {
    let mut registry = Registry::default();
    super::super::register(&mut registry);
    m.cpu.set_reg(Reg::Esp, STACK);
    m.cpu.write_u32(STACK, 0x12345678).unwrap();
    for (index, &value) in args.iter().enumerate() {
        m.cpu
            .write_u32(STACK + 4 + index as u32 * 4, value)
            .unwrap();
    }
    (registry.get("d3d9.dll", name).unwrap().handler)(&mut env(m))
}
fn okay(outcome: Outcome) {
    assert!(matches!(outcome, Outcome::Return(0)));
}
fn packet<T: bytemuck::Pod>(m: &mut Machine<JitCpu>, value: T) -> u32 {
    let mut e = env(m);
    let pointer = alloc(&mut e, std::mem::size_of::<T>() as u32).unwrap();
    GuestPtr::new(pointer).write(e.cpu, value).unwrap();
    pointer
}
fn texture(m: &mut Machine<JitCpu>, device: u32, levels: u32, pool: u32) -> u32 {
    let output = packet(m, 0u32);
    okay(call(
        m,
        "IDirect3DDevice9::CreateTexture",
        &[device, 4, 4, levels, 0, 21, pool, output, 0],
    ));
    m.cpu.read_u32(output).unwrap()
}
fn lock(
    m: &mut Machine<JitCpu>,
    texture: u32,
    level: u32,
    rect: u32,
    flags: u32,
) -> D3DLOCKED_RECT {
    let output = packet(m, D3DLOCKED_RECT::default());
    okay(call(
        m,
        "IDirect3DTexture9::LockRect",
        &[texture, level, output, rect, flags],
    ));
    GuestPtr::new(output).read(&m.cpu).unwrap()
}
fn add_rect(m: &mut Machine<JitCpu>, texture: u32, rect: u32) -> Outcome {
    call(m, "IDirect3DTexture9::AddDirtyRect", &[texture, rect])
}
fn unlock(m: &mut Machine<JitCpu>, texture: u32, level: u32) -> Outcome {
    call(m, "IDirect3DTexture9::UnlockRect", &[texture, level])
}
fn image(m: &mut Machine<JitCpu>, texture: u32, level: usize) -> Image {
    let e = env(m);
    let surface = resources::resource(&e, texture).unwrap().levels[level]
        .surface
        .unwrap();
    e.ext.get::<Surfaces>().targets[&surface].image.clone()
}
fn pixels(m: &mut Machine<JitCpu>, image: &Image) -> Vec<u8> {
    let mut transfer = m
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
fn submissions(m: &Machine<JitCpu>) -> u64 {
    m.ext
        .get::<State>()
        .gpu
        .as_ref()
        .unwrap()
        .stats()
        .submissions
}

#[test]
fn system_memory_dirty_metadata_is_shared_and_bad_rectangles_do_not_mark_or_allocate() {
    let (mut m, device) = machine(false);
    let texture = texture(&mut m, device, 3, 2);
    let partial = packet(
        &mut m,
        RECT {
            left: 1,
            top: 1,
            right: 3,
            bottom: 3,
        },
    );
    let aliases = m
        .sys
        .objects
        .alias_count(m.ext.get::<State>().com.namespace());
    let objects = m.sys.objects.object_count();
    for rect in [
        RECT {
            left: -1,
            top: 0,
            right: 3,
            bottom: 3,
        },
        RECT {
            left: 0,
            top: 0,
            right: 5,
            bottom: 3,
        },
        RECT {
            left: 2,
            top: 2,
            right: 2,
            bottom: 4,
        },
    ] {
        let rect = packet(&mut m, rect);
        let e = env(&mut m);
        if let Some(Payload::Resource(r)) = com(&e).get_mut::<Payload>(&mut e.sys.objects, texture)
        {
            for level in &mut r.levels {
                level.dirty = false;
            }
        }
        assert!(matches!(
            add_rect(&mut m, texture, rect),
            Outcome::Return(INVALID)
        ));
        assert!(
            resources::resource(&env(&mut m), texture)
                .unwrap()
                .levels
                .iter()
                .all(|level| !level.dirty)
        );
    }
    for (this, rect) in [(texture, 1), (texture, 0x551000), (device, 0), (0, 0)] {
        assert!(matches!(
            add_rect(&mut m, this, rect),
            Outcome::Return(INVALID)
        ));
    }
    let locked = lock(&mut m, texture, 0, partial, 0x8000);
    m.cpu.write(locked.pBits, &[1, 2, 3, 4]).unwrap();
    okay(add_rect(&mut m, texture, partial));
    assert!(
        resources::resource(&env(&mut m), texture)
            .unwrap()
            .levels
            .iter()
            .all(|level| level.dirty)
    );
    assert!(
        resources::resource(&env(&mut m), texture).unwrap().levels[0]
            .lock
            .is_some()
    );
    okay(unlock(&mut m, texture, 0));
    okay(add_rect(&mut m, texture, 0));
    assert_eq!(m.sys.objects.object_count(), objects);
    assert_eq!(
        m.sys
            .objects
            .alias_count(m.ext.get::<State>().com.namespace()),
        aliases
    );
    assert!(m.ext.get::<Surfaces>().targets.is_empty());
    assert_eq!(release(&mut env(&mut m), texture), 0);
    let mut registry = Registry::default();
    register(&mut registry);
    assert_eq!(
        registry
            .get("d3d9.dll", "IDirect3DTexture9::AddDirtyRect")
            .unwrap()
            .conv,
        adv_machine::Conv::Stdcall { argc: 2 }
    );
    assert_eq!(
        registry.level("d3d9.dll", "IDirect3DTexture9::AddDirtyRect"),
        Some(ImplementationLevel::Implement)
    );
    assert!(registry.signature_conflicts().is_empty());
}

#[test]
fn managed_no_dirty_lock_defers_explicit_updates_until_unlock_and_no_mark_keeps_gpu_unchanged() {
    let (mut m, device) = machine(true);
    let texture = texture(&mut m, device, 1, 1);
    let target = image(&mut m, texture, 0);
    let storage = resources::resource(&env(&mut m), texture).unwrap().storage;
    let mut expected = [0x11u8, 0x22, 0x33, 0x44].repeat(16);
    m.cpu.write(storage, &expected).unwrap();
    okay(add_rect(&mut m, texture, 0));
    assert_eq!(pixels(&mut m, &target), expected);
    let partial = packet(
        &mut m,
        RECT {
            left: 1,
            top: 1,
            right: 3,
            bottom: 3,
        },
    );
    let locked = lock(&mut m, texture, 0, partial, 0x8000);
    let before = submissions(&m);
    for row in 0..2 {
        m.cpu
            .write(locked.pBits + row * locked.Pitch as u32, &[0xaa; 8])
            .unwrap();
        expected[(row as usize + 1) * 16 + 4..(row as usize + 1) * 16 + 12].fill(0xaa);
    }
    okay(add_rect(&mut m, texture, partial));
    assert_eq!(
        submissions(&m),
        before,
        "locked AddDirtyRect must not submit GPU work"
    );
    assert_eq!(
        pixels(&mut m, &target),
        [0x11u8, 0x22, 0x33, 0x44].repeat(16)
    );
    okay(unlock(&mut m, texture, 0));
    assert_eq!(pixels(&mut m, &target), expected);
    assert!(
        resources::resource(&env(&mut m), texture)
            .unwrap()
            .levels
            .iter()
            .all(|level| !level.dirty && level.lock.is_none())
    );

    let locked = lock(&mut m, texture, 0, 0, 0x8000);
    m.cpu.write(locked.pBits, &[7; 4]).unwrap();
    let before = submissions(&m);
    okay(unlock(&mut m, texture, 0));
    assert_eq!(submissions(&m), before);
    assert_eq!(pixels(&mut m, &target), expected);
    okay(add_rect(&mut m, texture, 0));
    expected[..4].fill(7);
    assert_eq!(pixels(&mut m, &target), expected);
    lock(&mut m, texture, 0, 0, 0x10);
    let before = submissions(&m);
    okay(unlock(&mut m, texture, 0));
    assert_eq!(submissions(&m), before, "READONLY must not auto-mark");
    assert_eq!(release(&mut env(&mut m), texture), 0);
    assert!(m.ext.get::<Surfaces>().targets.is_empty());
}

#[test]
fn mip_dirty_expansion_waits_for_last_lock_and_failed_storage_read_can_be_retried() {
    let (mut m, device) = machine(true);
    let texture = texture(&mut m, device, 3, 1);
    let images = [
        image(&mut m, texture, 0),
        image(&mut m, texture, 1),
        image(&mut m, texture, 2),
    ];
    for (level, value) in [(0, 0x11), (1, 0x22), (2, 0x33)] {
        let locked = lock(&mut m, texture, level, 0, 0x8000);
        m.cpu
            .write(locked.pBits, &vec![value; (4usize >> level).pow(2) * 4])
            .unwrap();
    }
    okay(add_rect(&mut m, texture, 0));
    let before = submissions(&m);
    okay(unlock(&mut m, texture, 0));
    okay(unlock(&mut m, texture, 2));
    assert_eq!(submissions(&m), before);
    okay(unlock(&mut m, texture, 1));
    for (level, value) in [(0, 0x11), (1, 0x22), (2, 0x33)] {
        assert_eq!(
            pixels(&mut m, &images[level]),
            vec![value; (4usize >> level).pow(2) * 4]
        );
    }
    let storage = resources::resource(&env(&mut m), texture).unwrap().storage;
    lock(&mut m, texture, 0, 0, 0);
    let page = storage & !4095;
    m.cpu.protect(page, 4096, Prot(0)).unwrap();
    assert!(matches!(
        unlock(&mut m, texture, 0),
        Outcome::Return(INVALID)
    ));
    assert!(resources::resource(&env(&mut m), texture).unwrap().levels[0].dirty);
    m.cpu.protect(page, 4096, Prot::RW).unwrap();
    okay(unlock(&mut m, texture, 0));
    assert!(
        resources::resource(&env(&mut m), texture)
            .unwrap()
            .levels
            .iter()
            .all(|level| !level.dirty && level.lock.is_none())
    );
    assert_eq!(release(&mut env(&mut m), texture), 0);
}

#[test]
fn busy_upload_preserves_new_marks_and_cancellation_releases_only_the_reservation() {
    let (mut m, device) = machine(true);
    let texture = texture(&mut m, device, 1, 1);
    let target = image(&mut m, texture, 0);
    let storage = resources::resource(&env(&mut m), texture).unwrap().storage;
    m.cpu.write(storage, &[0x66; 64]).unwrap();
    let pause = m
        .ext
        .get::<State>()
        .gpu
        .as_ref()
        .unwrap()
        .context()
        .pause_submissions()
        .unwrap();
    let Outcome::Block { then, .. } = add_rect(&mut m, texture, 0) else {
        panic!("paused GPU must wait");
    };
    okay(add_rect(&mut m, texture, 0)); // Must survive the older pending upload.
    assert!(resources::resource(&env(&mut m), texture).unwrap().levels[0].dirty);
    drop(pause);
    assert!(super::super::pump(&mut m).unwrap());
    okay(then(&mut env(&mut m), Resume::Woke(WakeReason::Signaled)));
    assert_eq!(pixels(&mut m, &target), [0x66; 64]);
    assert!(!resources::resource(&env(&mut m), texture).unwrap().levels[0].dirty);
    assert!(m.ext.get::<State>().pending.is_empty());
    let objects = m.sys.objects.object_count();
    let pause = m
        .ext
        .get::<State>()
        .gpu
        .as_ref()
        .unwrap()
        .context()
        .pause_submissions()
        .unwrap();
    let outcome = add_rect(&mut m, texture, 0);
    assert!(matches!(outcome, Outcome::Block { .. }));
    super::super::cancel_pending(&mut m);
    drop(outcome);
    drop(pause);
    {
        let e = env(&mut m);
        let r = resources::resource(&e, texture).unwrap();
        assert!(r.levels[0].dirty);
        assert!(r.levels[0].lock.is_none());
    }
    assert_eq!(m.sys.objects.object_count(), objects);
    assert!(m.ext.get::<State>().pending.is_empty());
    okay(unlock(&mut m, texture, 0));
    assert_eq!(release(&mut env(&mut m), texture), 0);
}

#[test]
fn argb_font_coverage_survives_managed_upload_and_typed_draw_while_rgbx_is_opaque() {
    use adv_device::gpu::AlphaMode;
    for format in [21, 22, 26] {
        let (mut m, device_pointer) = machine(true);
        let output = packet(&mut m, 0u32);
        okay(call(
            &mut m,
            "IDirect3DDevice9::CreateTexture",
            &[device_pointer, 4, 4, 1, 0, format, 1, output, 0],
        ));
        let texture = m.cpu.read_u32(output).unwrap();
        let source = image(&mut m, texture, 0);
        assert_eq!(
            source.alpha_mode(),
            if format != 22 {
                AlphaMode::Stored
            } else {
                AlphaMode::Opaque
            }
        );
        let locked = lock(&mut m, texture, 0, 0, 0x8000);
        let coverage = if format == 26 { 136 } else { 128 };
        let row = [
            255u8, 255, 255, 0, 255, 255, 255, coverage, 255, 255, 255, 255, 0, 0, 0, 0,
        ];
        let data = if format == 26 {
            assert_eq!(locked.Pitch, 8);
            [0xff, 0x0f, 0xff, 0x8f, 0xff, 0xff, 0, 0].repeat(4)
        } else {
            assert_eq!(locked.Pitch, 16);
            row.repeat(4)
        };
        m.cpu.write(locked.pBits, &data).unwrap();
        okay(add_rect(&mut m, texture, 0));
        okay(unlock(&mut m, texture, 0));
        let mut expected_upload = row.repeat(4);
        if format == 22 {
            for pixel in expected_upload.chunks_exact_mut(4) {
                pixel[3] = 255;
            }
        }
        assert_eq!(pixels(&mut m, &source), expected_upload);

        let target = m
            .ext
            .get::<State>()
            .gpu
            .as_ref()
            .unwrap()
            .target(4, 4, false)
            .unwrap()
            .with_alpha_mode(AlphaMode::Opaque);
        let target_surface = {
            let surfaces = m.ext.get_mut::<Surfaces>();
            surfaces.next += 1;
            surfaces.targets.insert(surfaces.next, target.clone());
            surfaces.next
        };
        {
            let e = env(&mut m);
            let owner = com(&e)
                .identity::<Payload>(&e.sys.objects, device_pointer)
                .unwrap();
            let back = e
                .sys
                .objects
                .hosts_in::<adv_os::com::ComObject<Payload>>(com(&e).namespace())
                .find_map(|(id, _)| {
                    matches!(com(&e).get_identity(&e.sys.objects, id),
                    Some(Payload::Resource(r)) if r.kind == Kind::Surface)
                    .then_some(id)
                })
                .unwrap();
            if let Some(Payload::Resource(r)) = com(&e).get_identity_mut(&mut e.sys.objects, back) {
                r.surface = Some(target_surface);
                r.desc.Format = 22;
                r.desc.Width = 4;
                r.desc.Height = 4;
            }
            if let Some(Payload::Device(d)) = com(&e).get_identity_mut(&mut e.sys.objects, owner) {
                d.surface = target_surface;
                d.back_buffer = Some(back);
                d.fixed.target_changed(4, 4);
            }
        }
        okay(call(
            &mut m,
            "IDirect3DDevice9::Clear",
            &[device_pointer, 0, 0, 1, 0xff0000ff, 1f32.to_bits(), 0],
        ));
        okay(call(
            &mut m,
            "IDirect3DDevice9::SetTexture",
            &[device_pointer, 0, texture],
        ));
        okay(call(
            &mut m,
            "IDirect3DDevice9::SetFVF",
            &[device_pointer, 0x144],
        ));
        for (state, value) in [(27, 1), (19, 5), (20, 6), (22, 1)] {
            okay(call(
                &mut m,
                "IDirect3DDevice9::SetRenderState",
                &[device_pointer, state, value],
            ));
        }
        // The observed font path modulates both channels against white diffuse.
        okay(call(
            &mut m,
            "IDirect3DDevice9::SetTextureStageState",
            &[device_pointer, 0, 4, 4],
        ));
        let mut vertices = [0u32; 28];
        for (index, (x, y, u, v)) in [
            (-0.5f32, -0.5f32, 0f32, 0f32),
            (3.5, -0.5, 1., 0.),
            (-0.5, 3.5, 0., 1.),
            (3.5, 3.5, 1., 1.),
        ]
        .into_iter()
        .enumerate()
        {
            vertices[index * 7..index * 7 + 7].copy_from_slice(&[
                x.to_bits(),
                y.to_bits(),
                0,
                1f32.to_bits(),
                u32::MAX,
                u.to_bits(),
                v.to_bits(),
            ]);
        }
        let vertices = packet(&mut m, vertices);
        okay(call(
            &mut m,
            "IDirect3DDevice9::BeginScene",
            &[device_pointer],
        ));
        okay(call(
            &mut m,
            "IDirect3DDevice9::DrawPrimitiveUP",
            &[device_pointer, 5, 2, vertices, 28],
        ));
        okay(call(
            &mut m,
            "IDirect3DDevice9::EndScene",
            &[device_pointer],
        ));
        let drawn = pixels(&mut m, &target.image);
        for row in drawn.chunks_exact(16) {
            if format != 22 {
                assert_eq!(
                    &row[..3],
                    &[255, 0, 0],
                    "transparent white must preserve blue background"
                );
                assert_eq!(row[4], 255);
                assert!(row[5].abs_diff(coverage) <= 1 && row[6].abs_diff(coverage) <= 1);
                assert_eq!(&row[8..11], &[255; 3]);
                assert_eq!(&row[12..15], &[255, 0, 0]);
            } else {
                assert_eq!(&row[..3], &[255; 3]);
                assert_eq!(&row[4..7], &[255; 3]);
                assert_eq!(&row[8..11], &[255; 3]);
                assert_eq!(&row[12..15], &[0; 3]);
            }
        }
        okay(call(
            &mut m,
            "IDirect3DDevice9::SetTexture",
            &[device_pointer, 0, 0],
        ));
        assert_eq!(release(&mut env(&mut m), texture), 0);
    }
}

#[test]
fn packed_argb_mip_and_surface_alias_locks_share_two_byte_storage_and_deferred_uploads() {
    let (mut m, device) = machine(true);
    let output = packet(&mut m, 0u32);
    okay(call(
        &mut m,
        "IDirect3DDevice9::CreateTexture",
        &[device, 4, 4, 3, 0, 26, 1, output, 0],
    ));
    let texture = m.cpu.read_u32(output).unwrap();
    let levels = resources::resource(&env(&mut m), texture)
        .unwrap()
        .levels
        .clone();
    assert_eq!(
        levels.iter().map(|level| level.offset).collect::<Vec<_>>(),
        [0, 32, 40]
    );
    let mut locks = Vec::new();
    for (level, width) in [4u32, 2, 1].into_iter().enumerate() {
        let locked = lock(&mut m, texture, level as u32, 0, 0x8000);
        assert_eq!(locked.Pitch, (width * 2) as i32);
        assert_eq!(
            locked.pBits,
            resources::resource(&env(&mut m), texture).unwrap().storage + levels[level].offset
        );
        m.cpu
            .write(locked.pBits, &[0x23, 0x81].repeat((width * width) as usize))
            .unwrap();
        locks.push(locked);
    }
    let submitted = submissions(&m);
    okay(add_rect(&mut m, texture, 0));
    okay(unlock(&mut m, texture, 0));
    okay(unlock(&mut m, texture, 1));
    assert_eq!(
        submissions(&m),
        submitted,
        "a remaining mip lock prevents upload"
    );
    okay(unlock(&mut m, texture, 2));
    for (level, width) in [4u32, 2, 1].into_iter().enumerate() {
        let image = image(&mut m, texture, level);
        assert_eq!(
            pixels(&mut m, &image),
            [51, 34, 17, 136].repeat((width * width) as usize)
        );
    }
    okay(call(
        &mut m,
        "IDirect3DTexture9::GetSurfaceLevel",
        &[texture, 0, output],
    ));
    let alias = m.cpu.read_u32(output).unwrap();
    let rect = packet(
        &mut m,
        RECT {
            left: 1,
            top: 1,
            right: 3,
            bottom: 3,
        },
    );
    let locked_output = packet(&mut m, D3DLOCKED_RECT::default());
    okay(call(
        &mut m,
        "IDirect3DSurface9::LockRect",
        &[alias, locked_output, rect, 0x8000],
    ));
    let locked: D3DLOCKED_RECT = GuestPtr::new(locked_output).read(&m.cpu).unwrap();
    assert_eq!((locked.Pitch, locked.pBits), (8, locks[0].pBits + 10));
    assert!(matches!(
        call(
            &mut m,
            "IDirect3DTexture9::LockRect",
            &[texture, 0, locked_output, 0, 0]
        ),
        Outcome::Return(INVALID)
    ));
    m.cpu.write(locked.pBits, &[0x00, 0xff]).unwrap();
    let submitted = submissions(&m);
    okay(add_rect(&mut m, texture, rect));
    assert_eq!(submissions(&m), submitted);
    okay(call(&mut m, "IDirect3DSurface9::UnlockRect", &[alias]));
    let image = image(&mut m, texture, 0);
    let actual = pixels(&mut m, &image);
    assert_eq!(&actual[20..24], &[0, 0, 255, 255]);
    assert_eq!(&actual[..4], &[51, 34, 17, 136]);
    let desc_output = packet(&mut m, adv_abi::types::D3DSURFACE_DESC::default());
    okay(call(
        &mut m,
        "IDirect3DSurface9::GetDesc",
        &[alias, desc_output],
    ));
    let desc: adv_abi::types::D3DSURFACE_DESC = GuestPtr::new(desc_output).read(&m.cpu).unwrap();
    assert_eq!((desc.Format, desc.Width, desc.Height), (26, 4, 4));
    assert_eq!(release(&mut env(&mut m), alias), 1);
    assert_eq!(release(&mut env(&mut m), texture), 0);
}

#[test]
fn packed_argb_rejects_render_targets_and_keeps_offscreen_guest_locks_packed() {
    let (mut m, device) = machine(false);
    let output = packet(&mut m, 0xdeadbeefu32);
    assert!(matches!(
        call(
            &mut m,
            "IDirect3DDevice9::CreateTexture",
            &[device, 4, 4, 1, 1, 26, 0, output, 0]
        ),
        Outcome::Return(UNAVAILABLE)
    ));
    assert_eq!(m.cpu.read_u32(output).unwrap(), 0);
    okay(call(
        &mut m,
        "IDirect3DDevice9::CreateOffscreenPlainSurface",
        &[device, 3, 2, 26, 2, output, 0],
    ));
    let surface = m.cpu.read_u32(output).unwrap();
    let rect = packet(
        &mut m,
        RECT {
            left: 1,
            top: 1,
            right: 3,
            bottom: 2,
        },
    );
    let locked_output = packet(&mut m, D3DLOCKED_RECT::default());
    let storage = resources::resource(&env(&mut m), surface).unwrap().storage;
    okay(call(
        &mut m,
        "IDirect3DSurface9::LockRect",
        &[surface, locked_output, rect, 0],
    ));
    let locked: D3DLOCKED_RECT = GuestPtr::new(locked_output).read(&m.cpu).unwrap();
    assert_eq!((locked.Pitch, locked.pBits), (6, storage + 8));
    m.cpu
        .write(locked.pBits, &[0x23, 0x81, 0xff, 0x0f])
        .unwrap();
    okay(call(&mut m, "IDirect3DSurface9::UnlockRect", &[surface]));
    okay(call(
        &mut m,
        "IDirect3DSurface9::LockRect",
        &[surface, locked_output, rect, 0x10],
    ));
    let mut actual = [0u8; 4];
    m.cpu.read(locked.pBits, &mut actual).unwrap();
    assert_eq!(actual, [0x23, 0x81, 0xff, 0x0f]);
    okay(call(&mut m, "IDirect3DSurface9::UnlockRect", &[surface]));
    assert!(m.ext.get::<State>().pending.is_empty());
    assert!(m.ext.get::<State>().transfers.is_empty());
    assert_eq!(release(&mut env(&mut m), surface), 0);
}

#[test]
fn a8_bytes_mip_aliases_identity_sampling_and_managed_restoration_match_native() {
    // Native authored a8-native/probe.c, SHA256
    // 2C24C2F0D023F65BEC88F4DBBA8F3C1389D544D85D06748776F4A4DE2E1CFC18.
    // Fixed COLOR/ALPHA SELECTTEXTURE returns zero RGB and the original alpha;
    // a borrowed surface write and an eviction preserve those same 16 pixels.
    use adv_device::gpu::AlphaMode;
    #[track_caller]
    fn okay(outcome: Outcome) {
        match outcome {
            Outcome::Return(value) => assert_eq!(value, 0, "A8 fixture API result"),
            _ => panic!("unexpected A8 fixture continuation"),
        }
    }
    let (mut m, device) = machine(true);
    let output = packet(&mut m, 0u32);
    let factory = create(&mut env(&mut m), 32).addr();
    for usage in [0, 0x200] {
        okay(call(
            &mut m,
            "IDirect3D9::CheckDeviceFormat",
            &[factory, 0, 1, 22, usage, 3, 28],
        ));
    }
    for (usage, kind) in [(1, 3), (0, 1), (0, 4), (0x400, 3), (0x100000, 3)] {
        assert!(matches!(
            call(
                &mut m,
                "IDirect3D9::CheckDeviceFormat",
                &[factory, 0, 1, 22, usage, kind, 28]
            ),
            Outcome::Return(UNAVAILABLE)
        ));
    }
    okay(call(
        &mut m,
        "IDirect3DDevice9::CreateTexture",
        &[device, 4, 4, 3, 0, 28, 1, output, 0],
    ));
    let texture = m.cpu.read_u32(output).unwrap();
    let storage = resources::resource(&env(&mut m), texture).unwrap().storage;
    let levels = resources::resource(&env(&mut m), texture)
        .unwrap()
        .levels
        .clone();
    assert_eq!(
        levels.iter().map(|level| level.offset).collect::<Vec<_>>(),
        [0, 16, 20]
    );
    let mut raw = vec![0u8, 64, 128, 255].repeat(4);
    let mip_data = [raw.clone(), vec![17, 85, 170, 255], vec![128]];
    for (level, side) in [4, 2, 1].into_iter().enumerate() {
        let locked = lock(&mut m, texture, level as u32, 0, 0);
        assert_eq!(locked.Pitch, side);
        assert_eq!(locked.pBits, storage + levels[level].offset);
        m.cpu.write(locked.pBits, &mip_data[level]).unwrap();
    }
    let before = submissions(&m);
    okay(unlock(&mut m, texture, 0));
    okay(unlock(&mut m, texture, 1));
    assert_eq!(submissions(&m), before);
    okay(unlock(&mut m, texture, 2));
    for (level, bytes) in mip_data.iter().enumerate() {
        let image = image(&mut m, texture, level);
        assert_eq!(image.alpha_mode(), AlphaMode::Stored);
        assert_eq!(
            pixels(&mut m, &image),
            bytes.iter().flat_map(|&a| [0, 0, 0, a]).collect::<Vec<_>>()
        );
    }
    okay(call(
        &mut m,
        "IDirect3DTexture9::GetSurfaceLevel",
        &[texture, 0, output],
    ));
    let alias = m.cpu.read_u32(output).unwrap();
    let rect = packet(
        &mut m,
        RECT {
            left: 1,
            top: 1,
            right: 3,
            bottom: 3,
        },
    );
    let lock_output = packet(&mut m, D3DLOCKED_RECT::default());
    okay(call(
        &mut m,
        "IDirect3DSurface9::LockRect",
        &[alias, lock_output, rect, 0],
    ));
    let locked: D3DLOCKED_RECT = GuestPtr::new(lock_output).read(&m.cpu).unwrap();
    assert_eq!((locked.Pitch, locked.pBits), (4, storage + 5));
    m.cpu.write(locked.pBits, &[17]).unwrap();
    raw[5] = 17;
    okay(call(&mut m, "IDirect3DSurface9::UnlockRect", &[alias]));
    let locked = lock(&mut m, texture, 0, 0, 0x10);
    let mut actual = [0; 16];
    m.cpu.read(locked.pBits, &mut actual).unwrap();
    assert_eq!(actual.as_slice(), raw);
    okay(unlock(&mut m, texture, 0));
    // Let the acquired surface reference be the texture's sole public owner;
    // the fixture's three-reference ceiling leaves room for normal binding.
    assert_eq!(release(&mut env(&mut m), texture), 1);

    // A driver may pad A8 rows. The conversion consumes alpha bytes only and
    // restores one guest byte per pixel, irrespective of canonical RGB.
    let (expanded, pitch) =
        format::upload(28, vec![0, 64, 128, 0xcc, 255, 17, 85], 4, 3, 2).unwrap();
    assert_eq!(pitch, 12);
    assert_eq!(
        expanded,
        [0, 64, 128, 255, 17, 85]
            .into_iter()
            .flat_map(|a| [0, 0, 0, a])
            .collect::<Vec<_>>()
    );
    assert_eq!(
        format::readback(28, expanded).unwrap(),
        [0, 64, 128, 255, 17, 85]
    );

    // The shared ownership fixture deliberately limits each object to three
    // references. Use its existing implicit back buffer, as the native probe
    // does, instead of adding a second RT resource beside the borrowed alias.
    let target = m
        .ext
        .get::<State>()
        .gpu
        .as_ref()
        .unwrap()
        .target(4, 4, false)
        .unwrap()
        .with_alpha_mode(AlphaMode::Stored);
    let target_surface = {
        let surfaces = m.ext.get_mut::<Surfaces>();
        surfaces.next += 1;
        surfaces.targets.insert(surfaces.next, target.clone());
        surfaces.next
    };
    {
        let e = env(&mut m);
        let owner = com(&e).identity::<Payload>(&e.sys.objects, device).unwrap();
        let back = com(&e).get_identity(&e.sys.objects, owner).and_then(|payload| {
            if let Payload::Device(d) = payload { d.back_buffer } else { None }
        }).unwrap_or_else(|| e.sys.objects.hosts_in::<adv_os::com::ComObject<Payload>>(com(&e).namespace())
            .find_map(|(id, _)| matches!(com(&e).get_identity(&e.sys.objects, id), Some(Payload::Resource(r)) if r.kind == Kind::Surface && r.container.is_none()).then_some(id)).unwrap());
        if let Some(Payload::Resource(r)) = com(&e).get_identity_mut(&mut e.sys.objects, back) {
            r.surface = Some(target_surface);
            r.desc.Format = 21;
            r.desc.Pool = 0;
            r.desc.Usage = 1;
            r.desc.Width = 4;
            r.desc.Height = 4;
        }
        if let Some(Payload::Device(d)) = com(&e).get_identity_mut(&mut e.sys.objects, owner) {
            d.surface = target_surface;
            d.back_buffer = Some(back);
            d.fixed.target_changed(4, 4);
        }
    }
    okay(call(&mut m, "IDirect3DDevice9::SetFVF", &[device, 0x144]));
    for (state, value) in [(7, 0), (22, 1), (27, 0), (15, 0)] {
        okay(call(
            &mut m,
            "IDirect3DDevice9::SetRenderState",
            &[device, state, value],
        ));
    }
    for (state, value) in [(1, 2), (2, 2), (4, 2), (5, 2)] {
        okay(call(
            &mut m,
            "IDirect3DDevice9::SetTextureStageState",
            &[device, 0, state, value],
        ));
    }
    for (state, value) in [(5, 1), (6, 1), (7, 0)] {
        okay(call(
            &mut m,
            "IDirect3DDevice9::SetSamplerState",
            &[device, 0, state, value],
        ));
    }
    let mut vertices = [0u32; 28];
    for (index, (x, y, u, v)) in [
        (-0.5f32, -0.5f32, 0f32, 0f32),
        (3.5, -0.5, 1., 0.),
        (-0.5, 3.5, 0., 1.),
        (3.5, 3.5, 1., 1.),
    ]
    .into_iter()
    .enumerate()
    {
        vertices[index * 7..index * 7 + 7].copy_from_slice(&[
            x.to_bits(),
            y.to_bits(),
            0,
            1f32.to_bits(),
            u32::MAX,
            u.to_bits(),
            v.to_bits(),
        ]);
    }
    let vertices = packet(&mut m, vertices);
    for restored in [false, true] {
        if restored {
            okay(call(
                &mut m,
                "IDirect3DDevice9::EvictManagedResources",
                &[device],
            ));
            assert!(
                resources::resource(&env(&mut m), alias)
                    .unwrap()
                    .surface
                    .is_none()
            );
            assert_eq!(
                resources::resource(&env(&mut m), texture).unwrap().storage,
                storage
            );
        }
        okay(call(
            &mut m,
            "IDirect3DDevice9::SetTexture",
            &[device, 0, texture],
        ));
        let e = env(&mut m);
        assert_eq!(
            resources::resource(&e, alias).unwrap().surface,
            resources::resource(&e, texture).unwrap().levels[0].surface
        );
        okay(call(
            &mut m,
            "IDirect3DDevice9::Clear",
            &[device, 0, 0, 1, u32::MAX, 1f32.to_bits(), 0],
        ));
        okay(call(&mut m, "IDirect3DDevice9::BeginScene", &[device]));
        okay(call(
            &mut m,
            "IDirect3DDevice9::DrawPrimitiveUP",
            &[device, 5, 2, vertices, 28],
        ));
        okay(call(&mut m, "IDirect3DDevice9::EndScene", &[device]));
        assert_eq!(
            pixels(&mut m, &target.image),
            raw.iter().flat_map(|&a| [0, 0, 0, a]).collect::<Vec<_>>()
        );
        okay(call(
            &mut m,
            "IDirect3DDevice9::SetTexture",
            &[device, 0, 0],
        ));
    }
    assert!(matches!(
        call(
            &mut m,
            "IDirect3DDevice9::CreateTexture",
            &[device, 4, 4, 1, 1, 28, 0, output, 0]
        ),
        Outcome::Return(UNAVAILABLE)
    ));
    assert_eq!(m.cpu.read_u32(output).unwrap(), 0);
    assert!(m.ext.get::<State>().pending.is_empty());
    assert!(m.ext.get::<State>().transfers.is_empty());
    assert_eq!(release(&mut env(&mut m), alias), 0);
    assert_eq!(release(&mut env(&mut m), factory), 0);
    assert_eq!(release(&mut env(&mut m), device), 0);
    assert!(m.ext.get::<Surfaces>().targets.is_empty());
}
