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
fn validate(
    env: &mut Env<'_>,
    parameters: D3DPRESENT_PARAMETERS,
    source: GuestPtr<RECT>,
    destination: GuestPtr<RECT>,
    window: u32,
    dirty: GuestPtr<()>,
) -> Result<(), u32> {
    super::validate(env, parameters, source, destination, window, dirty).map(|_| ())
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
fn profile_can_withhold_copy_but_keeps_discard() {
    let mut machine = super::super::tests::machine();
    let provider = machine.ext.get::<State>().provider;
    install(&mut machine, provider).unwrap();
    assert!(machine.ext.get::<State>().copy);
    machine.ext.get_mut::<State>().copy = false;
    let env = env(&mut machine);
    assert_eq!(normalize(&env, parameters(), 0), Err(UNAVAILABLE));
    let discard = D3DPRESENT_PARAMETERS {
        SwapEffect: 1,
        ..parameters()
    };
    assert_eq!(normalize(&env, discard, 0).unwrap().SwapEffect, 1);
    let reason = presentation_unavailable(parameters(), false).unwrap();
    assert!(reason.contains("SwapEffect=0x3 (3)"), "{reason}");
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
fn copy_rectangles_and_dirty_hints_validate_permissions_and_reject_stretch_or_another_window() {
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

#[test]
fn equal_rectangles_clip_source_and_destination_together_without_shifting_pixels() {
    let mut machine = super::super::tests::machine();
    let mut env = env(&mut machine);
    for (source, destination, transfer) in [
        (
            RECT {
                left: -2,
                top: -1,
                right: 5,
                bottom: 4,
            },
            RECT {
                left: 11,
                top: 8,
                right: 18,
                bottom: 13,
            },
            Some(Transfer {
                source: Rect {
                    x: 0,
                    y: 0,
                    w: 3,
                    h: 3,
                },
                x: 13,
                y: 9,
            }),
        ),
        (
            RECT {
                left: 3,
                top: 2,
                right: 7,
                bottom: 6,
            },
            RECT {
                left: -1,
                top: -2,
                right: 3,
                bottom: 2,
            },
            Some(Transfer {
                source: Rect {
                    x: 4,
                    y: 4,
                    w: 3,
                    h: 2,
                },
                x: 0,
                y: 0,
            }),
        ),
        (
            RECT {
                left: 40,
                top: 40,
                right: 44,
                bottom: 44,
            },
            RECT {
                left: 0,
                top: 0,
                right: 4,
                bottom: 4,
            },
            None,
        ),
        (
            RECT {
                left: i32::MIN,
                top: i32::MIN,
                right: i32::MAX,
                bottom: i32::MAX,
            },
            RECT {
                left: i32::MIN,
                top: i32::MIN,
                right: i32::MAX,
                bottom: i32::MAX,
            },
            Some(Transfer {
                source: Rect {
                    x: 0,
                    y: 0,
                    w: 16,
                    h: 12,
                },
                x: 0,
                y: 0,
            }),
        ),
    ] {
        let source = packet(&mut env, source);
        let destination = packet(&mut env, destination);
        assert_eq!(
            super::validate(
                &mut env,
                parameters(),
                source,
                destination,
                0,
                GuestPtr::new(0)
            ),
            Ok(Some(Copy {
                width: 16,
                height: 12,
                transfer
            }))
        );
    }
    assert!(env.ext.try_get::<ApiDiagnostics>().is_none());
}

#[derive(Default)]
struct Surfaces {
    next: u32,
    serial: u64,
    targets: BTreeMap<u32, Target>,
    frames: Vec<NativeFrame>,
}
fn pixels(env: &mut Env<'_>, image: &Image) -> Vec<u8> {
    super::super::tests::flush_recordings(env);
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

fn install_surfaces(env: &mut Env<'_>) {
    let gpu = pollster::block_on(Gpu::request(!cfg!(any(
        target_os = "macos",
        target_os = "android"
    ))))
    .expect("software COPY GPU fixture");
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
    state.provider.present_copy = |ext, surface, _, gpu| {
        let surfaces = ext.get_mut::<Surfaces>();
        let serial = surfaces.serial + 1;
        let frame = gpu.present(&surfaces.targets[&surface].image, serial)?;
        surfaces.frames.push(frame);
        surfaces.serial = serial;
        Ok(serial)
    };
    state.provider.present = |ext, surface, _, gpu| {
        let copy = ext.get::<State>().provider.present_copy;
        copy(ext, surface, None, gpu)
    };
    state.provider.presentation_serial = |ext| Some(ext.get::<Surfaces>().serial);
    state.provider.repeat = |ext, _| {
        let s = ext.get_mut::<Surfaces>();
        s.serial += 1;
        Ok(s.serial)
    };
}

#[test]
fn copy_create_present_retains_pixels_and_reset_retains_effect_using_one_gpu() {
    let mut machine = super::super::tests::machine();
    let mut env = env(&mut machine);
    install_surfaces(&mut env);
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
    let front = device(&env, pointer).unwrap().front.unwrap();
    assert_ne!(front, surface);
    assert_eq!(env.ext.get::<Surfaces>().targets.len(), 2);
    // Rendering after Present changes the original target but not its frame snapshot.
    assert!(matches!(
        super::super::clear(
            &mut env,
            GuestPtr::new(pointer),
            0,
            GuestPtr::new(0),
            1,
            0xff00ff00,
            1.0,
            0
        ),
        Outcome::Return(0)
    ));
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
    input
        .write(
            env.cpu,
            D3DPRESENT_PARAMETERS {
                BackBufferCount: 2,
                ..replacement
            },
        )
        .unwrap();
    assert!(matches!(
        reset(&mut env, GuestPtr::new(pointer), input),
        Outcome::Return(INVALID)
    ));
    assert_eq!(device(&env, pointer).unwrap().front, Some(front));
    let front_target = env.ext.get::<Surfaces>().targets[&front].clone();
    assert_eq!(pixels(&mut env, &front_target.image), green);
    input.write(env.cpu, replacement).unwrap();
    assert!(matches!(
        reset(&mut env, GuestPtr::new(pointer), input),
        Outcome::Return(0)
    ));
    assert_eq!(device(&env, pointer).unwrap().parameters, replacement);
    assert_eq!(device(&env, pointer).unwrap().front, None);
    assert!(!env.ext.get::<Surfaces>().targets.contains_key(&front));
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
    assert_eq!(super::super::release(&mut env, pointer), 0);
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

#[test]
fn partial_copy_preserves_front_pixels_back_buffer_and_prior_snapshots() {
    let mut machine = super::super::tests::machine();
    let mut env = env(&mut machine);
    install_surfaces(&mut env);
    let (factory, _) = object(&mut env, "IDirect3D9", IID_FACTORY, Payload::Factory).unwrap();
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
    let back = device(&env, pointer).unwrap().surface;
    let target = env.ext.get::<Surfaces>().targets[&back].clone();
    let original = (0..16 * 12)
        .flat_map(|i| [i as u8, (i * 7) as u8, (i * 13) as u8, 255])
        .collect::<Vec<_>>();
    env.ext
        .get_mut::<State>()
        .gpu
        .as_mut()
        .unwrap()
        .upload(
            &target.image,
            Rect {
                x: 0,
                y: 0,
                w: 16,
                h: 12,
            },
            64,
            &original,
        )
        .unwrap();
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
    let front = device(&env, pointer).unwrap().front.unwrap();
    let front_target = env.ext.get::<Surfaces>().targets[&front].clone();
    let first_snapshot = env.ext.get::<Surfaces>().frames[0].image.clone();

    env.ext
        .get_mut::<State>()
        .gpu
        .as_mut()
        .unwrap()
        .clear(&target, Some([0.0, 1.0, 0.0, 1.0]), None)
        .unwrap();
    let top = packet(
        &mut env,
        RECT {
            left: 0,
            top: 0,
            right: 16,
            bottom: 8,
        },
    );
    assert!(matches!(
        present(
            &mut env,
            GuestPtr::new(pointer),
            top,
            top,
            Handle(0),
            GuestPtr::new(0)
        ),
        Outcome::Return(0)
    ));
    let mut expected = original.clone();
    expected[..16 * 8 * 4].copy_from_slice(&[0u8, 255, 0, 255].repeat(16 * 8));
    assert_eq!(pixels(&mut env, &front_target.image), expected);
    let second_snapshot = env.ext.get::<Surfaces>().frames[1].image.clone();
    assert_eq!(pixels(&mut env, &second_snapshot), expected);
    assert_eq!(pixels(&mut env, &first_snapshot), original);
    assert_eq!(
        pixels(&mut env, &target.image),
        [0u8, 255, 0, 255].repeat(16 * 12)
    );

    let changed = (0..16 * 12)
        .flat_map(|i| [(i * 11) as u8, 23, (255 - i) as u8, 255])
        .collect::<Vec<_>>();
    env.ext
        .get_mut::<State>()
        .gpu
        .as_mut()
        .unwrap()
        .upload(
            &target.image,
            Rect {
                x: 0,
                y: 0,
                w: 16,
                h: 12,
            },
            64,
            &changed,
        )
        .unwrap();
    let source = packet(
        &mut env,
        RECT {
            left: -2,
            top: -1,
            right: 5,
            bottom: 4,
        },
    );
    let destination = packet(
        &mut env,
        RECT {
            left: 11,
            top: 8,
            right: 18,
            bottom: 13,
        },
    );
    assert!(matches!(
        present(
            &mut env,
            GuestPtr::new(pointer),
            source,
            destination,
            Handle(0),
            GuestPtr::new(0)
        ),
        Outcome::Return(0)
    ));
    for y in 0..3 {
        let src = y * 16 * 4;
        let dst = ((y + 9) * 16 + 13) * 4;
        expected[dst..dst + 3 * 4].copy_from_slice(&changed[src..src + 3 * 4]);
    }
    assert_eq!(pixels(&mut env, &front_target.image), expected);
    assert_eq!(pixels(&mut env, &target.image), changed);
    assert_eq!(pixels(&mut env, &first_snapshot), original);
    assert_eq!(
        &pixels(&mut env, &second_snapshot)[16 * 8 * 4..],
        &original[16 * 8 * 4..]
    );

    let outside = packet(
        &mut env,
        RECT {
            left: 40,
            top: 40,
            right: 44,
            bottom: 44,
        },
    );
    let small = packet(
        &mut env,
        RECT {
            left: 0,
            top: 0,
            right: 4,
            bottom: 4,
        },
    );
    assert!(matches!(
        present(
            &mut env,
            GuestPtr::new(pointer),
            outside,
            small,
            Handle(0),
            GuestPtr::new(0)
        ),
        Outcome::Return(0)
    ));
    assert_eq!(pixels(&mut env, &front_target.image), expected);
    assert_eq!(env.ext.get::<Surfaces>().frames.len(), 4);
    assert_eq!(device(&env, pointer).unwrap().front, Some(front));
    assert_eq!(env.ext.get::<Surfaces>().targets.len(), 2);
    assert!(env.ext.try_get::<ApiDiagnostics>().is_none());
    assert_eq!(super::super::release(&mut env, pointer), 0);
    assert!(env.ext.get::<Surfaces>().targets.is_empty());
    assert_eq!(pixels(&mut env, &first_snapshot), original);
}
