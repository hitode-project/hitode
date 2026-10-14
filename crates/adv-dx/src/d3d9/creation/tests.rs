use super::super::shaders::{get_pixel, get_vertex, set_pixel, set_vertex};
use super::*;
use adv_cpu::backend::Guest;

#[test]
fn dialog_box_mode_is_inert_outside_scene_and_fullscreen_requires_a_gdi_back_buffer() {
    let mut m = super::super::tests::machine();
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (pointer, owner, back_pointer, back) = super::super::tests::setup(&mut env);
    let this = GuestPtr::new(pointer);
    if let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, owner) {
        d.parameters.Windowed = 1;
        d.parameters.BackBufferFormat = 21;
        d.parameters.Flags = 1;
        d.parameters.SwapEffect = 1;
        d.back_buffer = Some(back);
    }
    if let Some(Payload::Resource(r)) = com(&env).get_identity_mut(&mut env.sys.objects, back) {
        r.lock = Some(resources::Lock {
            rectangle: adv_device::surface::Rect {
                x: 0,
                y: 0,
                w: 1,
                h: 1,
            },
            flags: 0x10,
            ready: true,
        });
    }
    let aliases = env.sys.objects.alias_count(com(&env).namespace());
    let refs = env.sys.objects.strong_count(owner);
    for enabled in [0, 1] {
        assert_eq!(set_dialogs(&mut env, this, enabled), 0);
    }
    if let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, owner) {
        d.scene = true;
    }
    for enabled in [0, 1] {
        assert_eq!(set_dialogs(&mut env, this, enabled), INVALID);
    }
    if let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, owner) {
        d.scene = false;
        d.parameters.Windowed = 0;
    }
    // A fullscreen ARGB back buffer is not GDI-compatible; X8R8G8B8 is.
    assert_eq!(set_dialogs(&mut env, this, 1), INVALID);
    assert_eq!(set_dialogs(&mut env, this, 0), 0);
    if let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, owner) {
        d.parameters.BackBufferFormat = 22;
    }
    assert_eq!(set_dialogs(&mut env, this, 1), 0);
    assert_eq!(
        set_dialogs(&mut env, GuestPtr::new(back_pointer), 1),
        INVALID
    );
    assert_eq!(set_dialogs(&mut env, GuestPtr::new(0), 0), INVALID);
    assert!(
        resources::resource(&env, back_pointer)
            .unwrap()
            .lock
            .is_some()
    );
    assert_eq!(env.sys.objects.alias_count(com(&env).namespace()), aliases);
    assert_eq!(env.sys.objects.strong_count(owner), refs);
    assert!(env.ext.get::<super::super::State>().pending.is_empty());
    assert!(env.ext.get::<super::super::State>().transfers.is_empty());
}

#[test]
fn creation_rejects_missing_combined_and_unimplemented_policies_before_publication() {
    let mut m = super::super::tests::machine();
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (factory, id) = object(&mut env, "IDirect3D9", IID_FACTORY, Payload::Factory).unwrap();
    let output = GuestPtr::<u32>::new(alloc(&mut env, 4).unwrap());
    let count = env.sys.objects.alias_count(com(&env).namespace());
    for (flags, expected) in [
        (2, INVALID),
        (0x62, INVALID),
        (0xa2, INVALID),
        (0xc2, INVALID),
        (0xe2, INVALID),
        (0x32, INVALID),
        (0x92, INVALID),
        (0x52, UNAVAILABLE),
        (0x10022, UNAVAILABLE),
        (0x80000022, UNAVAILABLE),
    ] {
        output.write(env.cpu, 0xaabbccdd).unwrap();
        assert!(matches!(
            create_device(
                &mut env,
                GuestPtr::new(factory),
                0,
                1,
                Handle(0),
                flags,
                GuestPtr::new(0),
                output
            ),
            Outcome::Return(error) if error == expected
        ));
        assert_eq!(output.read(env.cpu).unwrap(), 0);
        assert_eq!(com(&env).references(&env.sys.objects, id), Some(1));
        assert_eq!(env.sys.objects.alias_count(com(&env).namespace()), count);
        assert!(env.ext.get::<super::super::State>().gpu.is_none());
    }
}

#[test]
fn fixed_modes_reject_changes_and_mixed_normalizes_bool_without_changing_creation_flags() {
    let mut m = super::super::tests::machine();
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (pointer, id, _, _) = super::super::tests::setup(&mut env);
    let output = GuestPtr::<D3DDEVICE_CREATION_PARAMETERS>::new(alloc(&mut env, 16).unwrap());
    for flags in [0x22, 0x42, 0x82, 0x26, 0x46, 0x86] {
        if let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, id) {
            d.creation = State::new(0, 1, 0, flags).unwrap();
        }
        let mut value = i32::from(flags & 0x20 != 0);
        assert_eq!(get_software(&mut env, GuestPtr::new(pointer)), value);
        for input in [0, 1, -1, 2, 0] {
            let expected = if flags & 0x80 == 0 && i32::from(input != 0) != value {
                INVALID
            } else {
                value = i32::from(input != 0);
                0
            };
            assert_eq!(
                set_software(&mut env, GuestPtr::new(pointer), input),
                expected
            );
            assert_eq!(get_software(&mut env, GuestPtr::new(pointer)), value);
            assert_eq!(parameters(&mut env, GuestPtr::new(pointer), output), 0);
            assert_eq!(
                output.read(env.cpu).unwrap(),
                D3DDEVICE_CREATION_PARAMETERS {
                    AdapterOrdinal: 0,
                    DeviceType: 1,
                    hFocusWindow: 0,
                    BehaviorFlags: flags,
                }
            );
        }
    }
}

#[test]
fn successful_reset_applies_mode_and_rejected_reset_keeps_previous_mode() {
    #[derive(Default)]
    struct Observed {
        modes: Vec<(u32, i32, u32, u32)>,
        released: Vec<u32>,
    }
    let mut m = super::super::tests::machine();
    let gpu = pollster::block_on(Gpu::request(!cfg!(any(
        target_os = "macos",
        target_os = "android"
    ))))
    .unwrap();
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (pointer, _, _, resource) = super::super::tests::setup(&mut env);
    let state = env.ext.get_mut::<super::super::State>();
    state.gpu = Some(gpu);
    state.provider.replace = |_, _, _| Ok(());
    state.provider.mode_changed = |env, owner, parameters| {
        env.ext.get_or_default::<Observed>().modes.push((
            owner,
            parameters.Windowed,
            parameters.BackBufferWidth,
            parameters.BackBufferHeight,
        ));
        Outcome::Return(0)
    };
    state.provider.device_released =
        |env, owner| env.ext.get_or_default::<Observed>().released.push(owner);
    let packet = GuestPtr::new(
        alloc(
            &mut env,
            std::mem::size_of::<D3DPRESENT_PARAMETERS>() as u32,
        )
        .unwrap(),
    );
    let mut parameters = D3DPRESENT_PARAMETERS {
        BackBufferWidth: 16,
        BackBufferHeight: 12,
        BackBufferFormat: 21,
        BackBufferCount: 1,
        SwapEffect: 1,
        hDeviceWindow: 0x7788,
        Windowed: 0,
        ..D3DPRESENT_PARAMETERS::default()
    };
    packet.write(env.cpu, parameters).unwrap();
    assert!(matches!(
        reset(&mut env, GuestPtr::new(pointer), packet),
        Outcome::Return(0)
    ));
    assert_eq!(env.ext.get::<Observed>().modes, [(pointer, 0, 16, 12)]);
    assert_eq!(begin_scene(&mut env, GuestPtr::new(pointer)), 0);
    parameters.Windowed = 1;
    parameters.BackBufferWidth = 32;
    packet.write(env.cpu, parameters).unwrap();
    assert!(matches!(
        reset(&mut env, GuestPtr::new(pointer), packet),
        Outcome::Return(INVALID)
    ));
    assert_eq!(env.ext.get::<Observed>().modes, [(pointer, 0, 16, 12)]);
    assert_eq!(end_scene(&mut env, GuestPtr::new(pointer)), 0);
    assert!(matches!(
        reset(&mut env, GuestPtr::new(pointer), packet),
        Outcome::Return(0)
    ));
    assert_eq!(
        env.ext.get::<Observed>().modes,
        [(pointer, 0, 16, 12), (pointer, 1, 32, 12)]
    );
    if let Some(Some(dead)) = com(&env).release_internal(&mut env.sys.objects, resource) {
        destroy(&mut env, dead);
    }
    assert_eq!(release(&mut env, pointer), 0);
    assert_eq!(env.ext.get::<Observed>().released, [pointer]);
}

#[test]
fn rejected_reset_keeps_mixed_mode_and_does_not_replace_creation_identity() {
    let mut m = super::super::tests::machine();
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (pointer, id, _, _) = super::super::tests::setup(&mut env);
    if let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, id) {
        d.creation = State::new(0, 1, 0x778899, 0x86).unwrap();
        d.scene = true;
    }
    assert_eq!(set_software(&mut env, GuestPtr::new(pointer), 1), 0);
    assert!(matches!(
        reset(&mut env, GuestPtr::new(pointer), GuestPtr::new(0)),
        Outcome::Return(INVALID)
    ));
    assert_eq!(get_software(&mut env, GuestPtr::new(pointer)), 1);
    let d = device(&env, pointer).unwrap();
    assert_eq!(d.creation.parameters.hFocusWindow, 0x778899);
    assert_eq!(d.creation.parameters.BehaviorFlags, 0x86);
}

#[test]
fn output_faults_and_nonnull_shader_rejection_preserve_memory_and_ownership() {
    let mut m = super::super::tests::machine();
    m.cpu.map(0x550000, 8192, Prot::RW).unwrap();
    m.cpu.write(0x550000, &[0xa5; 8192]).unwrap();
    m.cpu.protect(0x551000, 4096, Prot::READ).unwrap();
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (pointer, id, _, _) = super::super::tests::setup(&mut env);
    let references = com(&env).references(&env.sys.objects, id);
    let this = GuestPtr::new(pointer);
    assert_eq!(parameters(&mut env, this, GuestPtr::new(0x550ff8)), INVALID);
    let mut memory = [0; 16];
    env.cpu.read(0x550ff8, &mut memory).unwrap();
    assert_eq!(memory, [0xa5; 16]);
    assert_eq!(parameters(&mut env, this, GuestPtr::new(0)), INVALID);
    for setter in [set_vertex, set_pixel] {
        assert_eq!(setter(&mut env, this, GuestPtr::new(0xdeadbeef)), INVALID);
        assert_eq!(setter(&mut env, this, GuestPtr::new(0)), 0);
    }
    for getter in [get_vertex, get_pixel] {
        assert_eq!(getter(&mut env, this, GuestPtr::new(0x551000)), INVALID);
        assert_eq!(env.cpu.read_u32(0x551000).unwrap(), 0xa5a5a5a5);
        assert_eq!(getter(&mut env, this, GuestPtr::new(0)), INVALID);
        assert_eq!(getter(&mut env, this, GuestPtr::new(0x550000)), 0);
        assert_eq!(env.cpu.read_u32(0x550000).unwrap(), 0);
    }
    assert_eq!(com(&env).references(&env.sys.objects, id), references);
}
