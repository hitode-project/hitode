//! Guest-only lock publication and shared COM rollback, with no host GPU.
use super::*;
use adv_cpu::JitCpu;
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
    for flags in [0, 1, 2, 3] {
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
