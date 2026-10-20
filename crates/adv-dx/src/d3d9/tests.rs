//! Observable ownership edges measured by the native D3D9 fixture. This slice
//! isolates QueryInterface/limits without a renderer, window or guest execution.
use super::*;
use adv_abi::types::D3DSURFACE_DESC;
use adv_cpu::JitCpu;
use adv_machine::{Clock, ObjectLimits};

pub(super) fn machine() -> Machine<JitCpu> {
    machine_with_native(false)
}

pub(super) fn machine_with_native(native: bool) -> Machine<JitCpu> {
    machine_with_limits(native, 3)
}
fn machine_with_limits(native: bool, references: u32) -> Machine<JitCpu> {
    let cpu = JitCpu::new(adv_machine::layout::PARK_ADDR, native).unwrap();
    let mut m = Machine::new(cpu, Registry::default(), Clock::virtual_time()).unwrap();
    m.sys.objects = Objects::with_limits(ObjectLimits {
        references,
        ..ObjectLimits::default()
    });
    let mut heaps = Heaps::default();
    heaps.process_heap = heaps.create(&mut m.cpu, &mut m.sys.vm).unwrap();
    m.ext.insert(heaps);
    let com = Com::new(&mut m.sys.objects).unwrap();
    let provider = SurfaceProvider {
        acquire_dc: |_, _, _, _, _| Err("ownership-only fixture".into()),
        writeback_dc: |_, _, _, _| Err("ownership-only fixture".into()),
        release_dc: |_, _| {},
        insert: |_, _| unreachable!("ownership-only fixture"),
        target: |_, _| None,
        remove: |_, _, _| Ok(()),
        replace: |_, _, _| unreachable!(),
        present: |_, _, _, _| unreachable!(),
        present_copy: |_, _, _, _| unreachable!(),
        repeat: |_, _| unreachable!(),
        presentation_serial: |_| Some(0),
        vsync: |_| None,
        window: |_, _, _| Some((16, 12)),
        visible: |_, _, _| Some(false),
        fast_forward: |_| false,
        display: |_| (16, 12, 60),
        mode_changed: |_, _, _| Outcome::Return(0),
        device_released: |_, _| {},
        completed: |_, _| None,
        unpaced: |_| {},
    };
    m.ext.insert(State {
        com,
        tables: BTreeMap::from([
            ("IDirect3D9", 0),
            ("IDirect3DDevice9", 0),
            ("IDirect3DSurface9", 0),
            ("IDirect3DVertexBuffer9", 0),
            ("IDirect3DIndexBuffer9", 0),
            ("IDirect3DVertexDeclaration9", 0),
            ("IDirect3DVertexShader9", 0),
            ("IDirect3DPixelShader9", 0),
            ("IDirect3DStateBlock9", 0),
        ]),
        device_methods: Vec::new(),
        block_methods: Vec::new(),
        gpu: None,
        provider,
        pending: Vec::new(),
        transfers: Vec::new(),
        dcs: Vec::new(),
        sender: None,
        wake: None,
        copy: true,
        shader_caps: ShaderCaps::default(),
        shader_stats: ShaderStats::default(),
        shader_translations: shaders::TranslationCache::default(),
        programs: programmable::ProgramCache::default(),
        shader_cache: None,
        frames: FrameCounters::default(),
        verify_frames: false,
    });
    m
}

#[test]
fn child_frontend_inherits_host_policy_without_parent_guest_objects() {
    let mut parent = machine();
    let mut child = machine();
    parent.ext.get_mut::<State>().copy = false;
    set_shader_caps(
        &mut parent.ext,
        ShaderCaps {
            sm2: true,
            sm3: true,
        },
    );
    set_shader_cache(
        &mut parent.ext,
        Some(std::path::PathBuf::from("child-policy-cache")),
    );
    parent.ext.get_mut::<State>().shader_stats.vertex[4] = 9;
    let wake = adv_sync::WakeEvent::new().unwrap();
    parent.ext.get_mut::<State>().wake = Some(std::sync::Arc::new(wake));
    let child_com = child.ext.get::<State>().com.namespace();
    fork_host_state(&parent.ext, &mut child.ext).unwrap();
    let source = parent.ext.get::<State>();
    let destination = child.ext.get::<State>();
    assert!(!destination.copy);
    assert_eq!(destination.shader_caps, source.shader_caps);
    assert_eq!(destination.shader_cache, source.shader_cache);
    assert!(std::sync::Arc::ptr_eq(
        source.wake.as_ref().unwrap(),
        destination.wake.as_ref().unwrap()
    ));
    assert_eq!(destination.com.namespace(), child_com);
    assert_eq!(destination.shader_stats.vertex, [0; 7]);
    assert_eq!(destination.shader_stats.pixel, [0; 7]);
    assert!(
        destination.pending.is_empty()
            && destination.transfers.is_empty()
            && destination.dcs.is_empty()
    );
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
            front: None,
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
            shaders: shaders::Bindings::default(),
            recording: None,
            versions: [0; 15],
            frame: recording::Recording::default(),
            gpu_lost: false,
            boundary: None,
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
        context: machine.sys.current,
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
        context: m.sys.current,
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
        context: m.sys.current,
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

// spec: GFX-PACE-2
#[test]
fn base_present_completion_returns_success_without_changing_wait_policy() {
    struct Policy {
        held: bool,
        fence: bool,
        serial: u64,
        unpaced: u32,
    }
    let mut m = machine();
    m.ext.insert(Policy {
        held: false,
        fence: false,
        serial: 0,
        unpaced: 0,
    });
    let provider = &mut m.ext.get_mut::<State>().provider;
    provider.fast_forward = |ext| ext.get::<Policy>().held;
    provider.unpaced = |env| env.ext.get_mut::<Policy>().unpaced += 1;
    provider.completed = |env, serial| {
        let policy = env.ext.get_mut::<Policy>();
        policy.serial = serial;
        policy
            .fence
            .then(|| Outcome::block_then_return(Wait::Until(env.sys.clock.now_us() + 2_000), 0))
    };
    let mut env = Env {
        context: m.sys.current,
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
    // Unpaced frames reach the frame cap; paced and fast-forwarded ones do not.
    assert_eq!(env.ext.get::<Policy>().unpaced, 1);
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
    completed(&mut env, Completion::Presented(9, None));
    assert_eq!(env.ext.get::<Policy>().unpaced, 1);
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
        context: m.sys.current,
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
        context: m.sys.current,
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

/// Backend pixel assertions bypass guest readback APIs, so explicitly commit
/// recorded commands before asking Gpu for those pixels.
pub(in crate::d3d9) fn flush_recordings(env: &mut Env<'_>) {
    let ids: Vec<_> = env
        .sys
        .objects
        .hosts_in::<adv_os::com::ComObject<Payload>>(com(env).namespace())
        .filter_map(|(id, _)| {
            matches!(
                com(env).get_identity(&env.sys.objects, id),
                Some(Payload::Device(_))
            )
            .then_some(id)
        })
        .collect();
    for owner in ids {
        let token = 1;
        recording::flush(env, owner, token, recording::Reason::Readback).unwrap();
        recording::unlock(env, owner, token);
    }
}
/// Exercise the real Busy continuation through a recording capacity boundary.
pub(in crate::d3d9) fn force_recording_limit(env: &mut Env<'_>, pointer: u32) {
    let Some(Payload::Device(d)) = com(env).get_mut::<Payload>(&mut env.sys.objects, pointer)
    else {
        panic!()
    };
    d.frame.force_limit();
}

// spec: PROC-MACH-8
#[test]
fn shared_guest_counts_preserve_real_resource_parent_and_internal_lifetimes() {
    let mut m = machine_with_limits(false, u32::MAX);
    let mut env = Env {
        context: m.sys.current,
        cpu: &mut m.cpu,
        sys: &mut m.sys,
        ext: &mut m.ext,
        thunk: 0,
    };
    let (device, device_id, resource, id) = setup(&mut env);
    let count = env.cpu.read_u32(resource + 4).unwrap();
    assert_ne!(count, 0);
    assert_eq!(env.cpu.read_u32(count).unwrap(), 0);
    assert_eq!(add_ref(&mut env, resource), Some(1));
    assert_eq!(com(&env).references(&env.sys.objects, device_id), Some(2));
    // A guest AddRef changes the shared word without adding an Objects owner.
    env.cpu.write_u32(count, 2).unwrap();
    assert_eq!(release(&mut env, resource), 1);
    assert_eq!(com(&env).references(&env.sys.objects, device_id), Some(2));
    assert_eq!(release(&mut env, resource), 0);
    assert_eq!(com(&env).references(&env.sys.objects, device_id), Some(1));
    assert_eq!(com(&env).references(&env.sys.objects, id), Some(0));
    let dead = com(&env)
        .release_internal(&mut env.sys.objects, id)
        .unwrap()
        .unwrap();
    destroy(&mut env, dead);
    assert!(
        com(&env)
            .get::<Payload>(&env.sys.objects, resource)
            .is_none()
    );
    assert_eq!(release(&mut env, device), 0);
    assert_eq!(env.sys.objects.alias_count(com(&env).namespace()), 1);
}
