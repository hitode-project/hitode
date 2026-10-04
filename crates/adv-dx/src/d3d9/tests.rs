//! Observable ownership edges measured by the native D3D9 fixture. This slice
//! isolates QueryInterface/limits without a renderer, window or guest execution.
use super::*;
use adv_abi::types::D3DSURFACE_DESC;
use adv_cpu::JitCpu;
use adv_machine::{Clock, ObjectLimits};

pub(super) fn machine() -> Machine<JitCpu> {
    let cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap();
    let mut m = Machine::new(cpu, Registry::default(), Clock::virtual_time()).unwrap();
    m.sys.objects = Objects::with_limits(ObjectLimits {
        references: 3,
        ..ObjectLimits::default()
    });
    let mut heaps = Heaps::default();
    heaps.process_heap = heaps.create(&mut m.cpu, &mut m.sys.vm).unwrap();
    m.ext.insert(heaps);
    let com = Com::new(&mut m.sys.objects).unwrap();
    let provider = SurfaceProvider {
        insert: |_, _| unreachable!("ownership-only fixture"),
        target: |_, _| None,
        remove: |_, _, _| Ok(()),
        replace: |_, _, _| unreachable!(),
        present: |_, _, _| unreachable!(),
        window: |_, _, _| Some((16, 12)),
        visible: |_, _, _| Some(false),
        fast_forward: |_| false,
        display: |_| (16, 12, 60),
        completed: |_, _| None,
    };
    m.ext.insert(State {
        com,
        tables: BTreeMap::from([
            ("IDirect3D9", 0),
            ("IDirect3DDevice9", 0),
            ("IDirect3DSurface9", 0),
            ("IDirect3DVertexBuffer9", 0),
            ("IDirect3DIndexBuffer9", 0),
        ]),
        gpu: None,
        provider,
        pending: Vec::new(),
        transfers: Vec::new(),
        sender: None,
        wake: None,
    });
    m
}
pub(super) fn setup(env: &mut Env<'_>) -> (u32, Id, u32, Id) {
    let (_, factory) = object(env, "IDirect3D9", IID_FACTORY, Payload::Factory).unwrap();
    com(env)
        .add_ref_identity(&mut env.sys.objects, factory)
        .unwrap();
    let (device, device_id) = object(
        env,
        "IDirect3DDevice9",
        IID_DEVICE,
        Payload::Device(Box::new(Device {
            pointer: 0,
            factory,
            surface: 0,
            parameters: D3DPRESENT_PARAMETERS::default(),
            creator: 0,
            preserve_fpu: true,
            creation: creation::State::new(0, 1, 0, 0x22).unwrap(),
            scene: false,
            needs_reset: false,
            back_buffer: None,
            depth_stencil: None,
            texture: None,
            render_target: None,
            buffers: buffers::Bindings::default(),
            fixed: fixed::Fixed::new(16, 12),
        })),
    )
    .unwrap();
    if let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, device_id) {
        d.pointer = device;
    }
    let (resource, id) = resources::make(
        env,
        Resource {
            pointer: 0,
            device: device_id,
            kind: resources::Kind::Surface,
            desc: D3DSURFACE_DESC {
                Pool: 2,
                Type: 1,
                Width: 1,
                Height: 1,
                ..D3DSURFACE_DESC::default()
            },
            depth: None,
            surface: None,
            storage: 0,
            owns_surface: false,
            lock: None,
            container: None,
            levels: Vec::new(),
            level: 0,
            lod: 0,
        },
    )
    .unwrap();
    assert!(com(env).retain_internal(&mut env.sys.objects, id));
    assert_eq!(release(env, resource), 0);
    assert_eq!(com(env).references(&env.sys.objects, device_id), Some(1));
    (device, device_id, resource, id)
}
#[test]
fn native_sdk_31_and_32_create_real_factories_with_one_initial_reference() {
    let mut machine = machine();
    let mut env = Env {
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    for (version, supported) in [
        (0, false),
        (30, false),
        (31, true),
        (32, true),
        (33, false),
        (0x8000001f, false),
        (0x80000020, false),
        (u32::MAX, false),
    ] {
        let count = env.sys.objects.object_count();
        let pointer = create(&mut env, version).addr();
        assert_eq!(pointer != 0, supported, "SDK {version:#x}");
        if supported {
            assert!(matches!(
                com(&env).get::<Payload>(&env.sys.objects, pointer),
                Some(Payload::Factory)
            ));
            assert_eq!(env.sys.objects.object_count(), count + 1);
            assert_eq!(add_ref(&mut env, pointer), Some(2));
            assert_eq!(release(&mut env, pointer), 1);
            assert_eq!(release(&mut env, pointer), 0);
        }
        assert_eq!(env.sys.objects.object_count(), count);
    }
}

#[test]
fn query_of_bound_zero_reference_resource_retains_parent_once_and_failed_query_rolls_back() {
    let mut m = machine();
    let mut env = Env {
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (device, device_id, resource, id) = setup(&mut env);
    let guid = GuestPtr::<Iid>::new(alloc(&mut env, 16).unwrap());
    let output = GuestPtr::<u32>::new(alloc(&mut env, 4).unwrap());
    guid.write(env.cpu, adv_os::com::IUNKNOWN).unwrap();
    assert!(matches!(
        query(&mut env, GuestPtr::new(resource), guid, output),
        Outcome::Return(0)
    ));
    assert_eq!(output.read(env.cpu).unwrap(), resource);
    assert_eq!(com(&env).references(&env.sys.objects, device_id), Some(2));
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(1));
    assert_eq!(release(&mut env, resource), 0);
    guid.write(env.cpu, [0xf1; 16]).unwrap();
    assert!(matches!(
        query(&mut env, GuestPtr::new(resource), guid, output),
        Outcome::Return(NO_INTERFACE)
    ));
    assert_eq!(com(&env).references(&env.sys.objects, device_id), Some(1));
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(0));
    let dead = com(&env)
        .release_internal(&mut env.sys.objects, id)
        .unwrap()
        .unwrap();
    destroy(&mut env, dead);
    assert_eq!(release(&mut env, device), 0);
    assert_eq!(
        env.sys.objects.alias_count(com(&env).namespace()),
        1,
        "only original factory owner remains"
    );
}
#[test]
fn parent_reference_limit_refuses_resource_resurrection_without_mutating_counts() {
    let mut m = machine();
    let mut env = Env {
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (_device, device_id, resource, id) = setup(&mut env);
    assert_eq!(
        com(&env).add_ref_identity(&mut env.sys.objects, device_id),
        Some(2)
    );
    let guid = GuestPtr::<Iid>::new(alloc(&mut env, 16).unwrap());
    let output = GuestPtr::<u32>::new(alloc(&mut env, 4).unwrap());
    guid.write(env.cpu, adv_os::com::IUNKNOWN).unwrap();
    assert!(matches!(
        query(&mut env, GuestPtr::new(resource), guid, output),
        Outcome::Return(OUT_OF_MEMORY)
    ));
    assert_eq!(output.read(env.cpu).unwrap(), 0);
    assert_eq!(com(&env).references(&env.sys.objects, device_id), Some(2));
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(0));
    assert!(add_ref(&mut env, resource).is_none());
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(0));
}

#[test]
fn base_present_completion_returns_success_without_changing_wait_policy() {
    struct Policy {
        held: bool,
        fence: bool,
        serial: u64,
    }
    let mut m = machine();
    m.ext.insert(Policy {
        held: false,
        fence: false,
        serial: 0,
    });
    let provider = &mut m.ext.get_mut::<State>().provider;
    provider.fast_forward = |ext| ext.get::<Policy>().held;
    provider.completed = |env, serial| {
        let policy = env.ext.get_mut::<Policy>();
        policy.serial = serial;
        policy
            .fence
            .then(|| Outcome::block_then_return(Wait::Until(env.sys.clock.now_us() + 2_000), 0))
    };
    let mut env = Env {
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    // Native base Present returns D3D_OK even while hidden or minimized. These
    // states already suppress the refresh deadline; they do not imply 9Ex status.
    assert!(matches!(
        completed(&mut env, Completion::Presented(7, None)),
        Outcome::Return(0)
    ));
    assert_eq!(env.ext.get::<Policy>().serial, 7);
    let at = env.sys.clock.now_us() + 1_000;
    assert!(matches!(
        completed(&mut env, Completion::Presented(8, Some(at))),
        Outcome::Block { wait: Wait::Until(time), .. } if time == at
    ));
    env.ext.get_mut::<Policy>().held = true;
    assert!(matches!(
        completed(&mut env, Completion::Presented(9, Some(at))),
        Outcome::Return(0)
    ));
    env.ext.get_mut::<Policy>().fence = true;
    assert!(matches!(
        completed(&mut env, Completion::Presented(10, Some(at))),
        Outcome::Block { wait: Wait::Until(time), .. }
            if time == env.sys.clock.now_us() + 2_000
    ));
    assert_eq!(env.ext.get::<Policy>().serial, 10);
}

#[test]
fn get_direct3d_preserves_factory_identity_after_original_release_and_outlives_device() {
    let mut m = machine();
    m.sys.objects = Objects::with_limits(ObjectLimits {
        references: 8,
        ..ObjectLimits::default()
    });
    m.ext.get_mut::<State>().com = Com::new(&mut m.sys.objects).unwrap();
    let mut env = Env {
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (device, owner, _, back) = setup(&mut env);
    if let Some(Payload::Device(d)) = com(&env).get_identity_mut(&mut env.sys.objects, owner) {
        d.back_buffer = Some(back);
    }
    let factory = self::device(&env, device).unwrap().factory;
    let original = com(&env).canonical(&env.sys.objects, factory).unwrap();
    assert_eq!(
        release(&mut env, original),
        1,
        "device retains the actual creator"
    );
    let output = GuestPtr::<u32>::new(alloc(&mut env, 4).unwrap());
    assert_eq!(get_direct3d(&mut env, GuestPtr::new(device), output), 0);
    assert_eq!(output.read(env.cpu).unwrap(), original);
    assert_eq!(com(&env).references(&env.sys.objects, factory), Some(2));
    let iid = GuestPtr::<Iid>::new(alloc(&mut env, 16).unwrap());
    iid.write(env.cpu, adv_os::com::IUNKNOWN).unwrap();
    assert!(matches!(
        query(&mut env, GuestPtr::new(original), iid, output),
        Outcome::Return(0)
    ));
    assert_eq!(output.read(env.cpu).unwrap(), original);
    assert_eq!(release(&mut env, original), 2);
    assert_eq!(release(&mut env, device), 0);
    assert!(com(&env).get_identity(&env.sys.objects, owner).is_none());
    assert!(com(&env).get_identity(&env.sys.objects, back).is_none());
    assert_eq!(com(&env).references(&env.sys.objects, factory), Some(1));
    assert_eq!(adapter_count(&mut env, GuestPtr::new(original)), 1);
    assert_eq!(release(&mut env, original), 0);
    assert!(com(&env).canonical(&env.sys.objects, factory).is_none());
    assert_eq!(env.sys.objects.alias_count(com(&env).namespace()), 0);
}

#[test]
fn get_direct3d_output_fault_and_reference_limit_do_not_retain_or_partially_publish() {
    let mut m = machine();
    let mut env = Env {
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (device, owner, resource, _) = setup(&mut env);
    let factory = self::device(&env, device).unwrap().factory;
    let original = com(&env).canonical(&env.sys.objects, factory).unwrap();
    let objects = env.sys.objects.object_count();
    let refs = env.sys.objects.strong_count(factory);
    let page = env.sys.vm.alloc(env.cpu, 8192, Prot::RW).unwrap();
    let cross_page = GuestPtr::<u32>::new(page + 4094);
    cross_page.write(env.cpu, 0xdeadbeef).unwrap();
    env.cpu.protect(page + 4096, 4096, Prot::READ).unwrap();
    assert_eq!(
        get_direct3d(&mut env, GuestPtr::new(device), cross_page),
        INVALID
    );
    assert_eq!(cross_page.read(env.cpu).unwrap(), 0xdeadbeef);
    assert_eq!(env.sys.objects.strong_count(factory), refs);
    let output = GuestPtr::<u32>::new(page);
    output.write(env.cpu, 123).unwrap();
    assert_eq!(
        get_direct3d(&mut env, GuestPtr::new(device), output),
        OUT_OF_MEMORY
    );
    assert_eq!(output.read(env.cpu).unwrap(), 0);
    assert_eq!(env.sys.objects.strong_count(factory), refs);
    assert_eq!(env.sys.objects.object_count(), objects);
    assert_eq!(
        get_direct3d(&mut env, GuestPtr::new(resource), output),
        INVALID
    );
    assert_eq!(env.sys.objects.strong_count(factory), refs);
    assert_eq!(release(&mut env, original), 1);
    assert_eq!(get_direct3d(&mut env, GuestPtr::new(device), output), 0);
    assert_eq!(output.read(env.cpu).unwrap(), original);
    assert_eq!(com(&env).references(&env.sys.objects, owner), Some(1));
}
