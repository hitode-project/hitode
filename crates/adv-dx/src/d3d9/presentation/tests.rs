//! Authored creation/presentation requests over the existing single GPU owner.
use super::*;
use adv_abi::ApiDiagnostics;
use adv_cpu::{JitCpu, backend::Guest};
use adv_device::gpu::{Image, NativeFrame};

fn env(machine: &mut Machine<JitCpu>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn parameters() -> D3DPRESENT_PARAMETERS {
    D3DPRESENT_PARAMETERS {
        BackBufferWidth: 16,
        BackBufferHeight: 12,
        BackBufferFormat: 22,
        BackBufferCount: 1,
        SwapEffect: 3,
        hDeviceWindow: 0x7788,
        Windowed: 1,
        EnableAutoDepthStencil: 1,
        AutoDepthStencilFormat: 80,
        Flags: 1,
        ..D3DPRESENT_PARAMETERS::default()
    }
}
fn packet<T: bytemuck::Pod>(env: &mut Env<'_>, value: T) -> GuestPtr<T> {
    let pointer = GuestPtr::new(alloc(env, std::mem::size_of::<T>() as u32).unwrap());
    pointer.write(env.cpu, value).unwrap();
    pointer
}

#[test]
fn copy_parameters_preserve_effect_and_reject_multibuffer_or_multisample() {
    let mut machine = super::super::tests::machine();
    let env = env(&mut machine);
    for windowed in [0, 1] {
        for count in [0, 1] {
            let input = D3DPRESENT_PARAMETERS {
                Windowed: windowed,
                BackBufferCount: count,
                ..parameters()
            };
            let output = normalize(&env, input, 0).unwrap();
            assert_eq!(output.SwapEffect, 3);
            assert_eq!(output.BackBufferCount, 1);
            assert_eq!(output.Flags, 1);
            assert_eq!(output.AutoDepthStencilFormat, 80);
        }
    }
    for input in [
        D3DPRESENT_PARAMETERS {
            BackBufferCount: 2,
            ..parameters()
        },
        D3DPRESENT_PARAMETERS {
            MultiSampleType: 2,
            ..parameters()
        },
    ] {
        assert_eq!(normalize(&env, input, 0), Err(INVALID));
    }
    assert_eq!(
        normalize(
            &env,
            D3DPRESENT_PARAMETERS {
                SwapEffect: 2,
                ..parameters()
            },
            0
        ),
        Err(UNAVAILABLE)
    );
}

#[test]
fn unavailable_creation_reports_requested_fields_and_absent_gpu_without_publication() {
    let mut machine = super::super::tests::machine();
    let mut env = env(&mut machine);
    let (factory, id) = object(&mut env, "IDirect3D9", IID_FACTORY, Payload::Factory).unwrap();
    let input = D3DPRESENT_PARAMETERS {
        SwapEffect: 2,
        BackBufferCount: 2,
        MultiSampleType: 2,
        MultiSampleQuality: 3,
        Flags: 4,
        PresentationInterval: 2,
        BackBufferFormat: 23,
        AutoDepthStencilFormat: 99,
        ..parameters()
    };
    let input_pointer = packet(&mut env, input);
    let output = packet(&mut env, 0xaabbccddu32);
    let aliases = env.sys.objects.alias_count(com(&env).namespace());
    assert!(matches!(
        create_device(
            &mut env,
            GuestPtr::new(factory),
            0,
            1,
            Handle(0x7788),
            0x24,
            input_pointer,
            output
        ),
        Outcome::Return(UNAVAILABLE)
    ));
    assert_eq!(input_pointer.read(env.cpu).unwrap(), input);
    assert_eq!(output.read(env.cpu).unwrap(), 0);
    let reason = env
        .ext
        .get::<ApiDiagnostics>()
        .warnings
        .keys()
        .next()
        .unwrap()
        .1
        .clone();
    for field in [
        "SwapEffect=0x2 (2)",
        "BackBufferCount=0x2 (2)",
        "MultiSampleType=0x2 (2)",
        "MultiSampleQuality=0x3 (3)",
        "Flags=0x4 (4)",
        "PresentationInterval=0x2 (2)",
        "BackBufferFormat=0x17 (23)",
        "AutoDepthStencilFormat=0x63 (99)",
        "behavior_flags=0x24",
        "requested BackBufferWidth=16",
        "hDeviceWindow=0x7788",
        "Windowed=1",
    ] {
        assert!(reason.contains(field), "{field}: {reason}");
    }
    input_pointer.write(env.cpu, parameters()).unwrap();
    assert!(matches!(
        create_device(
            &mut env,
            GuestPtr::new(factory),
            0,
            1,
            Handle(0x7788),
            0x24,
            input_pointer,
            output
        ),
        Outcome::Return(UNAVAILABLE)
    ));
    assert!(
        env.ext
            .get::<ApiDiagnostics>()
            .warnings
            .keys()
            .any(
                |(_, reason)| reason.starts_with("GPU backend is not installed")
                    && reason.contains("SwapEffect=3")
            )
    );
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(1));
    assert_eq!(env.sys.objects.alias_count(com(&env).namespace()), aliases);
    assert_eq!(output.read(env.cpu).unwrap(), 0);
}

#[test]
fn copy_full_rectangles_and_dirty_hints_validate_permissions_and_reject_partial_composition() {
    let mut machine = super::super::tests::machine();
    machine.cpu.map(0x550000, 8192, Prot::RW).unwrap();
    machine.cpu.protect(0x551000, 4096, Prot::READ).unwrap();
    let mut env = env(&mut machine);
    let full = packet(
        &mut env,
        RECT {
            left: 0,
            top: 0,
            right: 16,
            bottom: 12,
        },
    );
    let partial = packet(
        &mut env,
        RECT {
            left: 2,
            top: 1,
            right: 5,
            bottom: 4,
        },
    );
    let region = packet(&mut env, [32u32, 1, 1, 16, 0, 0, 16, 12, 2, 1, 5, 4]);
    for (source, destination, window, dirty) in [
        (GuestPtr::new(0), GuestPtr::new(0), 0, GuestPtr::new(0)),
        (full, full, 0, GuestPtr::new(region.addr())),
        (full, full, 0x7788, GuestPtr::new(0)),
    ] {
        assert_eq!(
            validate(&mut env, parameters(), source, destination, window, dirty),
            Ok(())
        );
    }
    // A zero nRgnSize is allowed by RGNDATAHEADER; count still bounds the read.
    env.cpu.write_u32(region.addr() + 12, 0).unwrap();
    assert_eq!(
        validate(
            &mut env,
            parameters(),
            full,
            full,
            0,
            GuestPtr::new(region.addr())
        ),
        Ok(())
    );
    for (source, destination, window) in
        [(partial, full, 0), (full, partial, 0), (full, full, 0x9999)]
    {
        assert_eq!(
            validate(
                &mut env,
                parameters(),
                source,
                destination,
                window,
                GuestPtr::new(0)
            ),
            Err(UNAVAILABLE)
        );
    }
    for (source, destination, dirty) in [
        (GuestPtr::new(1), full, GuestPtr::new(0)),
        (full, GuestPtr::new(0x552000), GuestPtr::new(0)),
        (full, full, GuestPtr::new(0x550ff0)),
    ] {
        assert_eq!(
            validate(&mut env, parameters(), source, destination, 0, dirty),
            Err(INVALID)
        );
    }
    env.cpu.write_u32(region.addr() + 8, u32::MAX).unwrap();
    assert_eq!(
        validate(
            &mut env,
            parameters(),
            full,
            full,
            0,
            GuestPtr::new(region.addr())
        ),
        Err(INVALID)
    );
    let discard = D3DPRESENT_PARAMETERS {
        SwapEffect: 1,
        ..parameters()
    };
    assert_eq!(
        validate(&mut env, discard, full, full, 0, GuestPtr::new(0)),
        Err(INVALID)
    );
    assert_eq!(
        validate(
            &mut env,
            discard,
            GuestPtr::new(0),
            GuestPtr::new(0),
            0,
            GuestPtr::new(0)
        ),
        Ok(())
    );
}

#[derive(Default)]
struct Surfaces {
    next: u32,
    targets: BTreeMap<u32, Target>,
    frames: Vec<NativeFrame>,
}
fn pixels(env: &mut Env<'_>, image: &Image) -> Vec<u8> {
    let mut transfer = env
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

#[test]
fn copy_create_present_retains_pixels_and_reset_retains_effect_using_one_gpu() {
    let mut machine = super::super::tests::machine();
    let gpu = pollster::block_on(Gpu::request(true)).expect("software COPY GPU fixture");
    let mut env = env(&mut machine);
    env.ext.insert(Surfaces::default());
    let state = env.ext.get_mut::<State>();
    state.gpu = Some(gpu);
    state.provider.insert = |ext, target| {
        let surfaces = ext.get_mut::<Surfaces>();
        surfaces.next += 1;
        surfaces.targets.insert(surfaces.next, target);
        Ok(surfaces.next)
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
    state.provider.present = |ext, surface, gpu| {
        let surfaces = ext.get_mut::<Surfaces>();
        let serial = surfaces.frames.len() as u64 + 1;
        let frame = gpu.present(&surfaces.targets[&surface].image, serial)?;
        surfaces.frames.push(frame);
        Ok(serial)
    };
    let (factory, _) = object(&mut env, "IDirect3D9", IID_FACTORY, Payload::Factory).unwrap();
    let aliases = env.sys.objects.alias_count(com(&env).namespace());
    let input = packet(&mut env, parameters());
    let output = packet(&mut env, 0u32);
    assert!(matches!(
        create_device(
            &mut env,
            GuestPtr::new(factory),
            0,
            1,
            Handle(0x7788),
            0x24,
            input,
            output
        ),
        Outcome::Return(0)
    ));
    let pointer = output.read(env.cpu).unwrap();
    assert_ne!(pointer, 0);
    assert_eq!(device(&env, pointer).unwrap().parameters.SwapEffect, 3);
    assert_eq!(input.read(env.cpu).unwrap().SwapEffect, 3);
    let surface = device(&env, pointer).unwrap().surface;
    let target = env.ext.get::<Surfaces>().targets[&surface].clone();
    let expected = (0..16 * 12)
        .flat_map(|i| [i as u8, (i * 7) as u8, (i * 13) as u8, 255])
        .collect::<Vec<_>>();
    env.ext
        .get_mut::<State>()
        .gpu
        .as_mut()
        .unwrap()
        .upload(
            &target.image,
            adv_device::surface::Rect {
                x: 0,
                y: 0,
                w: 16,
                h: 12,
            },
            64,
            &expected,
        )
        .unwrap();
    assert_eq!(pixels(&mut env, &target.image), expected);
    assert!(matches!(
        present(
            &mut env,
            GuestPtr::new(pointer),
            GuestPtr::new(0),
            GuestPtr::new(0),
            Handle(0),
            GuestPtr::new(0)
        ),
        Outcome::Return(0)
    ));
    assert_eq!(pixels(&mut env, &target.image), expected);
    let snapshot = env.ext.get::<Surfaces>().frames[0].image.clone();
    assert_eq!(pixels(&mut env, &snapshot), expected);
    // Rendering after Present changes the original target but not its frame snapshot.
    env.ext
        .get_mut::<State>()
        .gpu
        .as_mut()
        .unwrap()
        .clear(&target, Some([0.0, 1.0, 0.0, 1.0]), None)
        .unwrap();
    let green = [0u8, 255, 0, 255].repeat(16 * 12);
    assert_eq!(pixels(&mut env, &target.image), green);
    assert_eq!(pixels(&mut env, &snapshot), expected);
    let full = packet(
        &mut env,
        RECT {
            left: 0,
            top: 0,
            right: 16,
            bottom: 12,
        },
    );
    let partial = packet(
        &mut env,
        RECT {
            left: 2,
            top: 1,
            right: 5,
            bottom: 4,
        },
    );
    assert!(matches!(
        present(
            &mut env,
            GuestPtr::new(pointer),
            partial,
            full,
            Handle(0),
            GuestPtr::new(0)
        ),
        Outcome::Return(UNAVAILABLE)
    ));
    assert_eq!(env.ext.get::<Surfaces>().frames.len(), 1);
    assert_eq!(pixels(&mut env, &target.image), green);
    assert!(matches!(
        present(
            &mut env,
            GuestPtr::new(pointer),
            full,
            full,
            Handle(0x7788),
            GuestPtr::new(0)
        ),
        Outcome::Return(0)
    ));
    assert_eq!(pixels(&mut env, &target.image), green);
    let replacement = D3DPRESENT_PARAMETERS {
        BackBufferWidth: 8,
        BackBufferHeight: 6,
        ..parameters()
    };
    input.write(env.cpu, replacement).unwrap();
    assert!(matches!(
        reset(&mut env, GuestPtr::new(pointer), input),
        Outcome::Return(0)
    ));
    assert_eq!(device(&env, pointer).unwrap().parameters, replacement);
    assert_eq!(input.read(env.cpu).unwrap(), replacement);
    let replacement_target = env.ext.get::<Surfaces>().targets[&surface].clone();
    assert_eq!(
        (
            replacement_target.image.width(),
            replacement_target.image.height()
        ),
        (8, 6)
    );
    assert!(!target.image.shares_storage(&replacement_target.image));
    let bad = D3DPRESENT_PARAMETERS {
        BackBufferCount: 2,
        ..replacement
    };
    input.write(env.cpu, bad).unwrap();
    assert!(matches!(
        reset(&mut env, GuestPtr::new(pointer), input),
        Outcome::Return(INVALID)
    ));
    assert_eq!(device(&env, pointer).unwrap().parameters, replacement);
    assert_eq!(release(&mut env, pointer), 0);
    assert_eq!(env.sys.objects.alias_count(com(&env).namespace()), aliases);
    assert!(env.ext.get::<Surfaces>().targets.is_empty());
    assert_eq!(
        env.ext
            .get::<State>()
            .gpu
            .as_ref()
            .unwrap()
            .stats()
            .native_frames,
        2
    );
    assert!(env.ext.get::<State>().pending.is_empty());
    assert!(env.ext.get::<State>().transfers.is_empty());
}
