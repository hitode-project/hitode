//! Guest-only lock publication and shared COM rollback, with no host GPU.
use super::*;
use adv_cpu::{JitCpu, backend::Guest};
use adv_machine::{ObjectLimits, Objects};

fn fixture(env: &mut Env<'_>) -> (u32, Id, u32, Id) {
    let (device, owner, pointer, id) = super::super::tests::setup(env);
    if let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, owner) {
        d.parameters.Flags = 1;
        d.back_buffer = Some(id);
    }
    if let Some(Payload::Resource(r)) = com(env).get_identity_mut(&mut env.sys.objects, id) {
        r.desc.Pool = 0;
        r.desc.Usage = 1;
        r.desc.Format = 21;
        r.desc.Width = 8;
        r.desc.Height = 6;
    }
    (device, owner, pointer, id)
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
fn pending(rectangle: Rect, flags: u32) -> Lock {
    Lock {
        rectangle,
        flags,
        ready: false,
    }
}

// spec: GFX-REC-10
#[test]
fn blocked_readback_keeps_pixels_from_before_a_later_write() {
    for busy in [false, true] {
        let mut f = super::super::stretch::tests::Fixture::new();
        f.m.ext
            .get_mut::<State>()
            .tables
            .insert("IDirect3DTexture9", 0);
        let src = f.render_target(4, 3, 21);
        let source = {
            let env = env(&mut f.m);
            let surface = resources::resource(&env, src).unwrap().surface.unwrap();
            (env.ext.get::<State>().provider.target)(env.ext, surface).unwrap()
        };
        let output = f.packet(0u32);
        let dst = if busy {
            f.okay(
                "IDirect3DDevice9::CreateOffscreenPlainSurface",
                &[f.device, 4, 3, 21, 2, output, 0],
            );
            f.m.cpu.read_u32(output).unwrap()
        } else {
            f.okay(
                "IDirect3DDevice9::CreateTexture",
                &[f.device, 4, 3, 1, 0, 21, 2, output, 0],
            );
            let texture = f.m.cpu.read_u32(output).unwrap();
            f.okay("IDirect3DTexture9::GetSurfaceLevel", &[texture, 0, output]);
            f.m.cpu.read_u32(output).unwrap()
        };
        let gpu = f.m.ext.get_mut::<State>().gpu.as_mut().unwrap();
        gpu.clear(&source, Some([1., 0., 0., 1.]), None).unwrap();
        let pause = busy.then(|| adv_device::gpu::test_support::pause_submissions(gpu).unwrap());
        let outcome = f.call(
            "IDirect3DDevice9::GetRenderTargetData",
            &[f.device, src, dst],
        );
        assert!(matches!(outcome, Outcome::Block { .. }));
        drop(pause);
        // Another guest context can submit a write before the first wait is serviced.
        let gpu = f.m.ext.get_mut::<State>().gpu.as_mut().unwrap();
        gpu.clear(&source, Some([0., 1., 0., 1.]), None).unwrap();
        gpu.flush().unwrap();
        assert_eq!(f.finish(outcome), 0);
        let locked = f.lock(dst, 0, 0x10);
        assert_eq!(
            f.m.cpu.read_u32(locked.pBits).unwrap(),
            0xffff0000,
            "busy={busy}"
        );
        f.okay("IDirect3DSurface9::UnlockRect", &[dst]);
    }
}

// spec: GFX-REC-10, GFX-REC-4
#[test]
fn readback_transfers_only_damaged_rows_and_columns() {
    let mut f = super::super::stretch::tests::Fixture::new();
    let (width, height) = (32, 24);
    let source = f.render_target(width, height, 21);
    let device = f.device;
    let output = f.packet(0u32);
    f.okay(
        "IDirect3DDevice9::CreateOffscreenPlainSurface",
        &[device, width, height, 21, 2, output, 0],
    );
    let destination = f.m.cpu.read_u32(output).unwrap();
    f.okay("IDirect3DDevice9::SetRenderTarget", &[device, 0, source]);
    f.okay(
        "IDirect3DDevice9::Clear",
        &[device, 0, 0, 1, 0xff0000ff, 0, 0],
    );

    let readback = |f: &mut super::super::stretch::tests::Fixture| {
        f.okay(
            "IDirect3DDevice9::GetRenderTargetData",
            &[device, source, destination],
        );
        f.lock(destination, 0, 0x10)
    };
    let baseline = readback(&mut f);
    assert_eq!(baseline.Pitch, (width * 4) as i32);
    assert_eq!(f.m.cpu.read_u32(baseline.pBits).unwrap(), 0xff0000ff);
    assert_eq!(
        f.m.ext.get::<State>().frames.readback_bytes,
        u64::from(width * height * 4)
    );
    f.okay("IDirect3DSurface9::UnlockRect", &[destination]);

    for rect in [[8i32, 0, 16, 8], [16, 16, 24, 24]] {
        let rectangle = f.packet(rect);
        f.okay(
            "IDirect3DDevice9::ColorFill",
            &[device, source, rectangle, 0xff00ff00],
        );
    }
    let bytes = f.m.ext.get::<State>().frames.readback_bytes;
    let locked = readback(&mut f);
    let transferred = f.m.ext.get::<State>().frames.readback_bytes - bytes;
    assert_eq!(transferred, 2 * 8 * 8 * 4);
    for y in 0..height {
        for x in 0..width {
            let changed = (8..16).contains(&x) && y < 8 || (16..24).contains(&x) && y >= 16;
            let pixel =
                f.m.cpu
                    .read_u32(locked.pBits + (y * width + x) * 4)
                    .unwrap();
            assert_eq!(
                pixel,
                if changed { 0xff00ff00 } else { 0xff0000ff },
                "({x}, {y})"
            );
        }
    }
    f.okay("IDirect3DSurface9::UnlockRect", &[destination]);
    readback(&mut f);
    assert_eq!(
        f.m.ext.get::<State>().frames.readback_bytes,
        bytes + transferred
    );
    f.okay("IDirect3DSurface9::UnlockRect", &[destination]);

    // Exposing writable CPU pixels invalidates the destination baseline.
    let locked = f.lock(destination, 0, 0);
    f.m.cpu.write_u32(locked.pBits, 0xffff0000).unwrap();
    f.okay("IDirect3DSurface9::UnlockRect", &[destination]);
    let locked = readback(&mut f);
    assert_eq!(f.m.cpu.read_u32(locked.pBits).unwrap(), 0xff0000ff);
    assert_eq!(
        f.m.ext.get::<State>().frames.readback_bytes,
        bytes + transferred + u64::from(width * height * 4)
    );
    f.okay("IDirect3DSurface9::UnlockRect", &[destination]);
}

// spec: GFX-COPY-1, GFX-COPY-2
#[test]
fn standalone_render_target_locks_read_gpu_pixels_and_upload_writes() {
    let mut f = super::super::stretch::tests::Fixture::new();
    f.m.ext
        .get_mut::<State>()
        .tables
        .insert("IDirect3DTexture9", 0);
    for format in [21, 22] {
        let output = f.packet(0u32);
        f.okay(
            "IDirect3DDevice9::CreateRenderTarget",
            &[f.device, 4, 3, format, 0, 0, 1, output, 0],
        );
        let target = f.m.cpu.read_u32(output).unwrap();
        f.okay(
            "IDirect3DDevice9::CreateTexture",
            &[f.device, 4, 3, 1, 1, format, 0, output, 0],
        );
        let texture = f.m.cpu.read_u32(output).unwrap();
        f.okay("IDirect3DTexture9::GetSurfaceLevel", &[texture, 0, output]);
        let source = f.m.cpu.read_u32(output).unwrap();
        f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, source]);
        f.okay(
            "IDirect3DDevice9::Clear",
            &[f.device, 0, 0, 1, 0xff123456, 1f32.to_bits(), 0],
        );
        f.okay(
            "IDirect3DDevice9::StretchRect",
            &[f.device, source, 0, target, 0, 0],
        );
        f.okay("IDirect3DDevice9::SetRenderTarget", &[f.device, 0, f.back]);
        for (api, pointer) in [
            ("IDirect3DSurface9::Release", source),
            ("IDirect3DTexture9::Release", texture),
        ] {
            let outcome = f.call(api, &[pointer]);
            f.finish(outcome);
        }
        let locked = f.lock(target, 0, 0x10);
        assert_eq!(locked.Pitch, 16);
        assert_eq!(f.m.cpu.read_u32(locked.pBits).unwrap(), 0xff123456);
        f.m.cpu.write_u32(locked.pBits, 0xffabcdef).unwrap();
        f.okay("IDirect3DSurface9::UnlockRect", &[target]);
        let locked = f.lock(target, 0, 0);
        assert_eq!(f.m.cpu.read_u32(locked.pBits).unwrap(), 0xff123456);
        f.m.cpu
            .write_u32(locked.pBits + 16 + 4, 0xffabcdef)
            .unwrap();
        f.okay("IDirect3DSurface9::UnlockRect", &[target]);
        let locked = f.lock(target, 0, 0x10);
        assert_eq!(f.m.cpu.read_u32(locked.pBits).unwrap(), 0xff123456);
        assert_eq!(f.m.cpu.read_u32(locked.pBits + 16 + 4).unwrap(), 0xffabcdef);
        f.okay("IDirect3DSurface9::UnlockRect", &[target]);

        let target = f.render_target(4, 3, format);
        let output = f.packet(D3DLOCKED_RECT::default());
        let outcome = f.call("IDirect3DSurface9::LockRect", &[target, output, 0, 0x10]);
        assert_eq!(f.finish(outcome), INVALID);
    }
}

// spec: GFX-SWAP-1
#[test]
fn actual_windowed_lockable_parameters_preserve_flags_and_use_client_size() {
    let mut m = super::super::tests::machine();
    let env = env(&mut m);
    let parameters = D3DPRESENT_PARAMETERS {
        BackBufferFormat: 21,
        BackBufferCount: 1,
        SwapEffect: 1,
        hDeviceWindow: 0x10020,
        Windowed: 1,
        Flags: 1,
        ..D3DPRESENT_PARAMETERS::default()
    };
    for flags in [0, 1, 2, 3, 0x10, 0x11, 0x12, 0x13] {
        let normalized = normalize(
            &env,
            D3DPRESENT_PARAMETERS {
                Flags: flags,
                ..parameters
            },
            0,
        )
        .unwrap();
        assert_eq!(
            (normalized.BackBufferWidth, normalized.BackBufferHeight),
            (16, 12)
        );
        assert_eq!(normalized.Flags, flags);
        assert_eq!(normalized.BackBufferFormat, 21);
    }
    for bad in [
        D3DPRESENT_PARAMETERS {
            Flags: 4,
            ..parameters
        },
        D3DPRESENT_PARAMETERS {
            MultiSampleType: 2,
            ..parameters
        },
    ] {
        assert!(matches!(normalize(&env, bad, 0), Err(UNAVAILABLE)));
    }
}

#[test]
fn output_range_failure_does_not_lock_or_allocate_and_pending_lock_cannot_unlock() {
    let mut m = super::super::tests::machine();
    let mut env = env(&mut m);
    let (_, owner, pointer, id) = fixture(&mut env);
    let output_page = env.sys.vm.alloc(env.cpu, 8192, Prot::RW).unwrap();
    let output = GuestPtr::new(output_page + 4092);
    env.cpu.write_u32(output.addr(), 0xdeadbeef).unwrap();
    env.cpu
        .protect(output_page + 4096, 4096, Prot::READ)
        .unwrap();
    assert!(matches!(
        lock(
            &mut env,
            GuestPtr::new(pointer),
            output,
            GuestPtr::new(0),
            0
        ),
        Outcome::Return(INVALID)
    ));
    assert_eq!(env.cpu.read_u32(output.addr()).unwrap(), 0xdeadbeef);
    let r = resources::resource(&env, pointer).unwrap();
    assert_eq!(r.storage, 0);
    assert!(r.lock.is_none());
    assert!(lockable_back_buffer(&env, pointer, r));
    if let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, owner) {
        d.parameters.Flags = 0;
    }
    assert!(!lockable_back_buffer(
        &env,
        pointer,
        resources::resource(&env, pointer).unwrap()
    ));
    set_lock(
        &mut env,
        id,
        Some(pending(
            Rect {
                x: 0,
                y: 0,
                w: 8,
                h: 6,
            },
            0x10,
        )),
    );
    assert!(matches!(
        unlock(&mut env, GuestPtr::new(pointer)),
        Outcome::Return(INVALID)
    ));
    assert!(
        !resources::resource(&env, pointer)
            .unwrap()
            .lock
            .unwrap()
            .ready
    );
}

#[test]
fn readonly_rect_publication_uses_full_pitch_and_unlock_retains_one_staging_allocation() {
    let mut m = super::super::tests::machine();
    let mut env = env(&mut m);
    let (_, _, pointer, id) = fixture(&mut env);
    let storage = alloc(&mut env, 8 * 6 * 4).unwrap();
    let output = GuestPtr::<D3DLOCKED_RECT>::new(alloc(&mut env, 8).unwrap());
    if let Some(Payload::Resource(r)) = com(&env).get_identity_mut(&mut env.sys.objects, id) {
        r.storage = storage;
    }
    let rectangle = Rect {
        x: 2,
        y: 1,
        w: 3,
        h: 3,
    };
    for _ in 0..3 {
        set_lock(&mut env, id, Some(pending(rectangle, 0x10)));
        assert_eq!(publish_lock(&mut env, id, output, 0), 0);
        let value = output.read(env.cpu).unwrap();
        assert_eq!((value.Pitch, value.pBits), (32, storage + 40));
        assert!(
            resources::resource(&env, pointer)
                .unwrap()
                .lock
                .unwrap()
                .ready
        );
        assert!(matches!(
            unlock(&mut env, GuestPtr::new(pointer)),
            Outcome::Return(0)
        ));
        assert_eq!(resources::resource(&env, pointer).unwrap().storage, storage);
    }
    assert!(env.ext.get::<State>().pending.is_empty());
    assert!(env.ext.get::<State>().transfers.is_empty());
    assert!(matches!(
        unlock(&mut env, GuestPtr::new(pointer)),
        Outcome::Return(INVALID)
    ));
}

#[test]
fn delayed_output_protection_and_cancel_clear_lock_without_changing_public_counts() {
    let mut m = super::super::tests::machine();
    let mut env = env(&mut m);
    let (_, owner, pointer, id) = fixture(&mut env);
    let page = env.sys.vm.alloc(env.cpu, 8192, Prot::RW).unwrap();
    let output = GuestPtr::<D3DLOCKED_RECT>::new(page + 4092);
    output
        .write(
            env.cpu,
            D3DLOCKED_RECT {
                Pitch: 123,
                pBits: 456,
            },
        )
        .unwrap();
    let objects = env.sys.objects.object_count();
    let owner_refs = env.sys.objects.strong_count(owner).unwrap();
    let resource_refs = env.sys.objects.strong_count(id).unwrap();
    let event = retain_transfer(&mut env, owner, id).unwrap();
    set_lock(
        &mut env,
        id,
        Some(pending(
            Rect {
                x: 0,
                y: 0,
                w: 8,
                h: 6,
            },
            0,
        )),
    );
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(0));
    env.cpu.protect(page + 4096, 4096, Prot::READ).unwrap();
    assert_eq!(publish_lock(&mut env, id, output, 0), INVALID);
    assert_eq!(env.cpu.read_u32(output.addr()).unwrap(), 123);
    release_transfer(&mut env, event, id, owner, true);
    assert!(resources::resource(&env, pointer).unwrap().lock.is_none());
    assert_eq!(env.sys.objects.object_count(), objects);
    assert_eq!(env.sys.objects.strong_count(owner), Some(owner_refs));
    assert_eq!(env.sys.objects.strong_count(id), Some(resource_refs));
    // The same cleanup is used when the owning context exits before map completion.
    let event = retain_transfer(&mut env, owner, id).unwrap();
    set_lock(
        &mut env,
        id,
        Some(pending(
            Rect {
                x: 0,
                y: 0,
                w: 8,
                h: 6,
            },
            0,
        )),
    );
    release_transfer(&mut env, event, id, owner, true);
    assert!(resources::resource(&env, pointer).unwrap().lock.is_none());
    assert_eq!(env.sys.objects.object_count(), objects);
}

#[test]
fn failed_second_retain_and_event_exhaustion_roll_back_existing_com_owners() {
    let mut m = super::super::tests::machine();
    let mut env = env(&mut m);
    let (_, owner, _, id) = fixture(&mut env);
    assert!(com(&env).retain_internal(&mut env.sys.objects, id));
    let counts = (
        env.sys.objects.strong_count(owner),
        env.sys.objects.strong_count(id),
    );
    assert_eq!(retain_transfer(&mut env, owner, id), Err(OUT_OF_MEMORY));
    assert_eq!(
        (
            env.sys.objects.strong_count(owner),
            env.sys.objects.strong_count(id)
        ),
        counts
    );

    let mut m = super::super::tests::machine();
    m.sys.objects = Objects::with_limits(ObjectLimits {
        objects: 3,
        references: 8,
        ..ObjectLimits::default()
    });
    m.ext.get_mut::<State>().com = Com::new(&mut m.sys.objects).unwrap();
    let mut env = self::env(&mut m);
    let (_, owner, pointer, id) = fixture(&mut env);
    let counts = (
        env.sys.objects.strong_count(owner),
        env.sys.objects.strong_count(id),
    );
    assert_eq!(retain_transfer(&mut env, owner, id), Err(OUT_OF_MEMORY));
    assert_eq!(
        (
            env.sys.objects.strong_count(owner),
            env.sys.objects.strong_count(id)
        ),
        counts
    );
    assert_eq!(env.sys.objects.object_count(), 3);
    assert!(resources::resource(&env, pointer).unwrap().lock.is_none());
}

// spec: GFX-COPY-2, GFX-REC-2
#[test]
fn cpu_surface_and_texture_level_locks_keep_pending_draws_recorded() {
    let mut f = super::super::stretch::tests::Fixture::new();
    f.m.ext
        .get_mut::<State>()
        .tables
        .insert("IDirect3DTexture9", 0);
    let surface = f.surface(4, 3, 21, 2);
    let output = f.packet(0u32);
    f.okay(
        "IDirect3DDevice9::CreateTexture",
        &[f.device, 4, 3, 1, 0, 21, 1, output, 0],
    );
    let texture = f.m.cpu.read_u32(output).unwrap();
    f.okay("IDirect3DTexture9::GetSurfaceLevel", &[texture, 0, output]);
    let level = f.m.cpu.read_u32(output).unwrap();
    f.okay(
        "IDirect3DDevice9::Clear",
        &[f.device, 0, 0, 1, 0xff123456, 1f32.to_bits(), 0],
    );
    let before = f.m.ext.get::<State>().frames.flushes;
    for pointer in [surface, level] {
        let locked = f.lock(pointer, 0, 0x10);
        assert_ne!(locked.pBits, 0);
        f.okay("IDirect3DSurface9::UnlockRect", &[pointer]);
        assert_eq!(f.m.ext.get::<State>().frames.flushes, before);
    }
    let target = f.packet(0u32);
    f.okay(
        "IDirect3DDevice9::CreateRenderTarget",
        &[f.device, 4, 3, 21, 0, 0, 1, target, 0],
    );
    let target = f.m.cpu.read_u32(target).unwrap();
    f.lock(target, 0, 0x10);
    assert_eq!(f.m.ext.get::<State>().frames.flushes[2], before[2] + 1);
    f.okay("IDirect3DSurface9::UnlockRect", &[target]);
}

// spec: GFX-REC-1
#[test]
fn front_buffer_data_captures_presented_pixels_and_validates_inputs() {
    let mut f = super::super::stretch::tests::Fixture::new();
    let (width, height) = (16, 12);
    let destination = f.surface(width, height, 21, 2);
    f.okay(
        "IDirect3DDevice9::Clear",
        &[f.device, 0, 0, 1, 0xff224466, 1f32.to_bits(), 0],
    );
    let outcome = f.call(
        "IDirect3DDevice9::GetFrontBufferData",
        &[f.device, 0, destination],
    );
    assert_eq!(f.finish(outcome), 0);
    let locked = f.lock(destination, 0, 0x10);
    assert_eq!(f.m.cpu.read_u32(locked.pBits).unwrap(), 0xff224466);
    f.okay("IDirect3DSurface9::UnlockRect", &[destination]);

    // Reject wrong swap-chain index, format, pool and size with D3DERR_INVALIDCALL.
    let bad_chain = f.call(
        "IDirect3DDevice9::GetFrontBufferData",
        &[f.device, 1, destination],
    );
    assert_eq!(f.finish(bad_chain), INVALID);
    let wrong_format = f.surface(width, height, 22, 2);
    let bad_format = f.call(
        "IDirect3DDevice9::GetFrontBufferData",
        &[f.device, 0, wrong_format],
    );
    assert_eq!(f.finish(bad_format), INVALID);
    let wrong_size = f.surface(8, 6, 21, 2);
    let bad_size = f.call(
        "IDirect3DDevice9::GetFrontBufferData",
        &[f.device, 0, wrong_size],
    );
    assert_eq!(f.finish(bad_size), INVALID);
    assert!(matches!(
        f.call("IDirect3DDevice9::GetNPatchMode", &[f.device]),
        Outcome::ReturnF32(v) if v == 0.0
    ));
}

// spec: GFX-CAP-1, GFX-COPY-1, GFX-COPY-2
#[test]
fn r5g6b5_render_targets_sample_opaque_and_read_back_packed() {
    let mut f = super::super::stretch::tests::Fixture::new();
    f.m.ext
        .get_mut::<State>()
        .tables
        .insert("IDirect3DTexture9", 0);
    let output = f.packet(0u32);
    f.okay(
        "IDirect3DDevice9::CreateTexture",
        &[f.device, 4, 3, 1, 1, 23, 0, output, 0],
    );
    let texture = f.m.cpu.read_u32(output).unwrap();
    f.okay("IDirect3DTexture9::GetSurfaceLevel", &[texture, 0, output]);
    let rt_surface = f.m.cpu.read_u32(output).unwrap();
    assert_eq!(
        f.image(rt_surface).alpha_mode(),
        adv_device::gpu::AlphaMode::Opaque
    );

    // Clear with zero alpha; sampling an R5G6B5 target must still produce alpha 1.0.
    f.okay(
        "IDirect3DDevice9::SetRenderTarget",
        &[f.device, 0, rt_surface],
    );
    f.okay(
        "IDirect3DDevice9::Clear",
        &[f.device, 0, 0, 1, 0x00123456, 1f32.to_bits(), 0],
    );

    let argb_target = f.render_target(4, 3, 21);
    f.okay(
        "IDirect3DDevice9::SetRenderTarget",
        &[f.device, 0, argb_target],
    );
    f.okay(
        "IDirect3DDevice9::Clear",
        &[f.device, 0, 0, 1, 0x00000000, 1f32.to_bits(), 0],
    );
    f.okay("IDirect3DDevice9::SetTexture", &[f.device, 0, texture]);
    f.okay("IDirect3DDevice9::SetFVF", &[f.device, 0x144]);
    f.okay("IDirect3DDevice9::SetRenderState", &[f.device, 27, 0]);
    f.okay("IDirect3DDevice9::BeginScene", &[f.device]);
    #[repr(C)]
    #[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
    struct QuadVertex {
        x: f32,
        y: f32,
        z: f32,
        rhw: f32,
        color: u32,
        u: f32,
        v: f32,
    }
    let vertices = f.packet([
        QuadVertex {
            x: -0.5,
            y: -0.5,
            z: 0.0,
            rhw: 1.0,
            color: 0xffff_ffff,
            u: 0.0,
            v: 0.0,
        },
        QuadVertex {
            x: 3.5,
            y: -0.5,
            z: 0.0,
            rhw: 1.0,
            color: 0xffff_ffff,
            u: 1.0,
            v: 0.0,
        },
        QuadVertex {
            x: -0.5,
            y: 2.5,
            z: 0.0,
            rhw: 1.0,
            color: 0xffff_ffff,
            u: 0.0,
            v: 1.0,
        },
        QuadVertex {
            x: 3.5,
            y: 2.5,
            z: 0.0,
            rhw: 1.0,
            color: 0xffff_ffff,
            u: 1.0,
            v: 1.0,
        },
    ]);
    f.okay(
        "IDirect3DDevice9::DrawPrimitiveUP",
        &[f.device, 5, 2, vertices, 28],
    );
    f.okay("IDirect3DDevice9::EndScene", &[f.device]);
    f.okay("IDirect3DDevice9::SetTexture", &[f.device, 0, 0]);
    let sampled = f.image(argb_target);
    assert_eq!(&f.pixels(&sampled)[..4], &[0x56, 0x34, 0x12, 0xff]);

    // Standalone lockable R5G6B5 render target + StretchRect + GetRenderTargetData.
    f.okay(
        "IDirect3DDevice9::CreateRenderTarget",
        &[f.device, 4, 3, 23, 0, 0, 1, output, 0],
    );
    let standalone = f.m.cpu.read_u32(output).unwrap();
    f.okay(
        "IDirect3DDevice9::StretchRect",
        &[f.device, rt_surface, 0, standalone, 0, 0],
    );
    let sysmem = f.surface(4, 3, 23, 2);
    f.okay(
        "IDirect3DDevice9::GetRenderTargetData",
        &[f.device, standalone, sysmem],
    );
    let expected = u16::from_le_bytes(format::pack(23, 0x00123456).unwrap().try_into().unwrap());
    let locked = f.lock(sysmem, 0, 0x10);
    assert_eq!(locked.Pitch, 8);
    assert_eq!(f.m.cpu.read_u16(locked.pBits).unwrap(), expected);
    f.okay("IDirect3DSurface9::UnlockRect", &[sysmem]);

    let locked = f.lock(standalone, 0, 0);
    assert_eq!(locked.Pitch, 8);
    assert_eq!(f.m.cpu.read_u16(locked.pBits).unwrap(), expected);
    f.m.cpu
        .write(locked.pBits + 2, &0xf81fu16.to_le_bytes())
        .unwrap();
    f.okay("IDirect3DSurface9::UnlockRect", &[standalone]);

    let locked = f.lock(standalone, 0, 0x10);
    assert_eq!(f.m.cpu.read_u16(locked.pBits).unwrap(), expected);
    assert_eq!(f.m.cpu.read_u16(locked.pBits + 2).unwrap(), 0xf81f);
    f.okay("IDirect3DSurface9::UnlockRect", &[standalone]);
}
