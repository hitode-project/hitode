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
    crate::test_support::install(&mut m);
    let com = Com::new(&mut m.sys.objects).unwrap();
    let provider = SurfaceProvider {
        allocator: test_allocator(),
        dc: test_dc(),
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
        monitor: 0x0001_0001,
        mode_changed: |_, _, _| Outcome::Return(0),
        device_released: |_, _| {},
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
            ("IDirect3DVertexDeclaration9", 0),
            ("IDirect3DVertexShader9", 0),
            ("IDirect3DPixelShader9", 0),
            ("IDirect3DStateBlock9", 0),
            ("IDirect3DSwapChain9", 0),
            ("IDirect3DQuery9", 0),
        ]),
        gpu: None,
        provider,
        pending: Vec::new(),
        private_releases: Vec::new(),
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
            children: BTreeMap::new(),
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
            swap_chain: None,
            queries: queries::DeviceState::default(),
            depth_stencil: None,
            texture: None,
            render_target: None,
            buffers: buffers::Bindings::default(),
            fixed: fixed::CachedFixed::new(16, 12),
            shaders: shaders::Bindings::default(),
            recording: None,
            versions: [0; 15],
            fingerprints: [0; 15],
            frame: recording::Recording::default(),
            gpu_lost: false,
            boundary: None,
            misc: Misc::default(),
        })),
    )
    .unwrap();
    if let Some(Payload::Device(d)) = com(env).get_identity_mut(&mut env.sys.objects, device_id) {
        d.pointer = device;
        d.refresh_all();
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
            lockable: false,
            lock: None,
            container: None,
            levels: Vec::new(),
            level: 0,
            lod: 0,
            common: resources::Common::default(),
            auto_filter: 2,
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
    guid.write(env.cpu, adv_abi::com::IUNKNOWN).unwrap();
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
    guid.write(env.cpu, adv_abi::com::IUNKNOWN).unwrap();
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

// spec: GFX-PACE-1
#[test]
fn base_present_completion_returns_success_without_changing_wait_policy() {
    struct Policy {
        held: bool,
        fence: bool,
        serial: u64,
    }
    let mut m = machine();
    m.spawn("present", adv_machine::layout::PARK_ADDR, &[], 0x10000)
        .unwrap();
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
    iid.write(env.cpu, adv_abi::com::IUNKNOWN).unwrap();
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
        .hosts_in::<adv_abi::com::ComObject<Payload>>(com(env).namespace())
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

pub(super) fn test_allocator() -> crate::interop::GuestAllocator {
    crate::interop::GuestAllocator {
        allocate: |env, length| {
            let heaps = env.ext.get_mut::<Heaps>();
            heaps.alloc(env.cpu, &mut env.sys.vm, heaps.process_heap, length, true)
        },
        free: |env, pointer| {
            let heaps = env.ext.get_mut::<Heaps>();
            heaps.free(env.cpu, &mut env.sys.vm, heaps.process_heap, pointer);
        },
    }
}
pub(super) fn test_dc() -> crate::interop::MemoryDc {
    crate::test_support::dc::provider()
}

mod package_a {
    use super::*;
    use adv_cpu::backend::{Guest, Reg};
    use adv_machine::Resume;

    const STACK: u32 = 0x550000;
    const ADD: u32 = 0x410000;
    const RELEASE: u32 = 0x410004;

    fn fixture() -> (Machine<JitCpu>, Registry, u32) {
        let mut m = machine_with_limits(false, u32::MAX);
        m.cpu.map(STACK, 4096, Prot::RW).unwrap();
        let (device, _, _, _) = setup(&mut env(&mut m));
        let mut registry = Registry::default();
        super::super::register(&mut registry);
        (m, registry, device)
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
    fn packet<T: bytemuck::Pod>(m: &mut Machine<JitCpu>, value: T) -> u32 {
        let mut e = env(m);
        let pointer = alloc(&mut e, std::mem::size_of::<T>() as u32).unwrap();
        GuestPtr::new(pointer).write(e.cpu, value).unwrap();
        pointer
    }
    fn call(m: &mut Machine<JitCpu>, registry: &Registry, name: &str, args: &[u32]) -> Outcome {
        m.cpu.set_reg(Reg::Esp, STACK);
        m.cpu.write_u32(STACK, 0).unwrap();
        for (i, &arg) in args.iter().enumerate() {
            m.cpu.write_u32(STACK + 4 + i as u32 * 4, arg).unwrap();
        }
        (registry.get("d3d9.dll", name).unwrap().handler)(&mut env(m))
    }
    // Model an application-owned IUnknown at its vtable boundary. This checks
    // actual requested calls, including callbacks made by object destruction.
    fn finish(m: &mut Machine<JitCpu>, outcome: Outcome) -> u32 {
        match outcome {
            Outcome::Return(value) => value,
            Outcome::Call {
                target, args, then, ..
            } => {
                assert_eq!(args.len(), 1);
                let count = m.cpu.read_u32(args[0] + 4).unwrap();
                let count = match target {
                    ADD => count + 1,
                    RELEASE => count - 1,
                    _ => panic!("unexpected COM method"),
                };
                m.cpu.write_u32(args[0] + 4, count).unwrap();
                let next = then(&mut env(m), Resume::Returned(count));
                finish(m, next)
            }
            _ => panic!("unexpected wait"),
        }
    }
    fn result(m: &mut Machine<JitCpu>, registry: &Registry, name: &str, args: &[u32]) -> u32 {
        let outcome = call(m, registry, name, args);
        finish(m, outcome)
    }
    fn surface(m: &mut Machine<JitCpu>, registry: &Registry, device: u32, pool: u32) -> u32 {
        let output = packet(m, 0u32);
        assert_eq!(
            result(
                m,
                registry,
                "IDirect3DDevice9::CreateOffscreenPlainSurface",
                &[device, 1, 1, 21, pool, output, 0]
            ),
            0
        );
        m.cpu.read_u32(output).unwrap()
    }

    // spec: GFX-RES-1
    #[test]
    fn package_a_resource_interfaces_share_implementations() {
        let (_, registry, _) = fixture();
        assert!(registry.signature_conflicts().is_empty());
        for method in [
            "GetDevice",
            "GetType",
            "SetPrivateData",
            "GetPrivateData",
            "FreePrivateData",
            "SetPriority",
            "GetPriority",
            "PreLoad",
        ] {
            let handler = registry
                .get("d3d9.dll", &format!("IDirect3DResource9::{method}"))
                .unwrap()
                .handler as usize;
            for interface in [
                "IDirect3DSurface9",
                "IDirect3DTexture9",
                "IDirect3DBaseTexture9",
                "IDirect3DVertexBuffer9",
                "IDirect3DIndexBuffer9",
            ] {
                assert_eq!(
                    registry
                        .get("d3d9.dll", &format!("{interface}::{method}"))
                        .unwrap()
                        .handler as usize,
                    handler
                );
            }
        }
        for method in [
            "SetLOD",
            "GetLOD",
            "GetLevelCount",
            "SetAutoGenFilterType",
            "GetAutoGenFilterType",
            "GenerateMipSubLevels",
        ] {
            assert_eq!(
                registry
                    .get("d3d9.dll", &format!("IDirect3DBaseTexture9::{method}"))
                    .unwrap()
                    .handler as usize,
                registry
                    .get("d3d9.dll", &format!("IDirect3DTexture9::{method}"))
                    .unwrap()
                    .handler as usize
            );
        }
    }

    // spec: GFX-RES-1
    #[test]
    fn package_a_private_bytes_copy_resize_replace_and_isolate() {
        let (mut m, registry, device) = fixture();
        let a = surface(&mut m, &registry, device, 2);
        let b = surface(&mut m, &registry, device, 2);
        let guid = packet(&mut m, [7u8; 16]);
        let data = packet(&mut m, [1u8, 2, 3, 4]);
        let output = packet(&mut m, [9u8; 8]);
        let size = packet(&mut m, 2u32);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DSurface9::SetPrivateData",
                &[a, guid, data, 4, 0]
            ),
            0
        );
        m.cpu.write_u32(data, 0).unwrap();
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DResource9::GetPrivateData",
                &[a, guid, output, size]
            ),
            0x88760867
        );
        assert_eq!(m.cpu.read_u32(size).unwrap(), 4);
        assert_eq!(m.cpu.read_u32(output).unwrap(), 0x09090909);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DSurface9::GetPrivateData",
                &[a, guid, 0, size]
            ),
            0
        );
        m.cpu.write_u32(size, 8).unwrap();
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DSurface9::GetPrivateData",
                &[a, guid, output, size]
            ),
            0
        );
        assert_eq!(m.cpu.read_u32(output).unwrap(), 0x04030201);
        assert_eq!(m.cpu.read_u32(size).unwrap(), 4);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DSurface9::GetPrivateData",
                &[b, guid, output, size]
            ),
            0x88760866
        );
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DResource9::SetPrivateData",
                &[a, guid, data, 1, 0]
            ),
            0
        );
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DSurface9::GetPrivateData",
                &[a, guid, output, size]
            ),
            0
        );
        assert_eq!(m.cpu.read_u32(size).unwrap(), 1);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DSurface9::FreePrivateData",
                &[a, guid]
            ),
            0
        );
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DSurface9::FreePrivateData",
                &[a, guid]
            ),
            0x88760866
        );
    }

    // spec: GFX-RES-1
    #[test]
    fn package_a_private_unknown_transfers_refs_and_dies_with_resource() {
        let (mut m, registry, device) = fixture();
        let resource = surface(&mut m, &registry, device, 2);
        let guid = packet(&mut m, [3u8; 16]);
        let table = packet(&mut m, [0u32, ADD, RELEASE]);
        let unknown = packet(&mut m, [table, 1u32]);
        let size = packet(&mut m, 4u32);
        let output = packet(&mut m, 0u32);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DSurface9::SetPrivateData",
                &[resource, guid, unknown, 4, 1]
            ),
            0
        );
        assert_eq!(m.cpu.read_u32(unknown + 4).unwrap(), 2);
        m.cpu.write_u32(size, 0).unwrap();
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DSurface9::GetPrivateData",
                &[resource, guid, output, size]
            ),
            0x88760867
        );
        assert_eq!(m.cpu.read_u32(unknown + 4).unwrap(), 2);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DSurface9::GetPrivateData",
                &[resource, guid, output, size]
            ),
            0
        );
        assert_eq!(m.cpu.read_u32(output).unwrap(), unknown);
        assert_eq!(m.cpu.read_u32(unknown + 4).unwrap(), 3);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DSurface9::SetPrivateData",
                &[resource, guid, unknown, 4, 1]
            ),
            0
        );
        assert_eq!(m.cpu.read_u32(unknown + 4).unwrap(), 3);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DSurface9::FreePrivateData",
                &[resource, guid]
            ),
            0
        );
        assert_eq!(m.cpu.read_u32(unknown + 4).unwrap(), 2);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DSurface9::SetPrivateData",
                &[resource, guid, unknown, 4, 1]
            ),
            0
        );
        assert_eq!(
            result(&mut m, &registry, "IDirect3DSurface9::Release", &[resource]),
            0
        );
        assert_eq!(m.cpu.read_u32(unknown + 4).unwrap(), 2);
        assert!(resources::resource(&env(&mut m), resource).is_none());
        assert!(m.ext.get::<State>().private_releases.is_empty());
    }

    // spec: GFX-RES-1
    #[test]
    fn package_a_private_unknown_survives_until_last_buffer_binding() {
        let (mut m, registry, device) = fixture();
        let output = packet(&mut m, 0u32);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::CreateVertexBuffer",
                &[device, 16, 0, 0, 1, output, 0]
            ),
            0
        );
        let buffer = m.cpu.read_u32(output).unwrap();
        let guid = packet(&mut m, [2u8; 16]);
        let table = packet(&mut m, [0u32, ADD, RELEASE]);
        let unknown = packet(&mut m, [table, 1u32]);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DVertexBuffer9::SetPrivateData",
                &[buffer, guid, unknown, 4, 1]
            ),
            0
        );
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::SetStreamSource",
                &[device, 0, buffer, 0, 4]
            ),
            0
        );
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DVertexBuffer9::Release",
                &[buffer]
            ),
            0
        );
        assert_eq!(m.cpu.read_u32(unknown + 4).unwrap(), 2);
        assert!(buffers::buffer(&env(&mut m), buffer).is_some());
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::SetStreamSource",
                &[device, 0, 0, 0, 0]
            ),
            0
        );
        assert_eq!(m.cpu.read_u32(unknown + 4).unwrap(), 1);
        assert!(buffers::buffer(&env(&mut m), buffer).is_none());
        assert!(m.ext.get::<State>().private_releases.is_empty());
    }

    // spec: GFX-RES-1
    #[test]
    fn package_a_priority_and_device_queries_cover_surfaces_and_buffers() {
        let (mut m, registry, device) = fixture();
        let surface = surface(&mut m, &registry, device, 2);
        let output = packet(&mut m, 0u32);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::CreateVertexBuffer",
                &[device, 16, 0, 0, 1, output, 0]
            ),
            0
        );
        let vertex = m.cpu.read_u32(output).unwrap();
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::CreateIndexBuffer",
                &[device, 16, 0, 101, 0, output, 0]
            ),
            0
        );
        let index = m.cpu.read_u32(output).unwrap();
        for (pointer, interface, kind, previous) in [
            (surface, "IDirect3DSurface9", 1, 7),
            (vertex, "IDirect3DVertexBuffer9", 6, 7),
            (index, "IDirect3DIndexBuffer9", 7, 0),
        ] {
            assert_eq!(
                result(
                    &mut m,
                    &registry,
                    &format!("{interface}::GetType"),
                    &[pointer]
                ),
                kind
            );
            assert_eq!(
                result(
                    &mut m,
                    &registry,
                    &format!("{interface}::SetPriority"),
                    &[pointer, 7]
                ),
                0
            );
            assert_eq!(
                result(
                    &mut m,
                    &registry,
                    &format!("{interface}::GetPriority"),
                    &[pointer]
                ),
                previous
            );
            assert_eq!(
                result(
                    &mut m,
                    &registry,
                    &format!("{interface}::SetPriority"),
                    &[pointer, 9]
                ),
                previous
            );
            assert_eq!(
                result(
                    &mut m,
                    &registry,
                    &format!("{interface}::PreLoad"),
                    &[pointer]
                ),
                0
            );
            let id = com(&env(&mut m))
                .identity::<Payload>(&m.sys.objects, device)
                .unwrap();
            let before = m
                .ext
                .get::<State>()
                .com
                .references(&m.sys.objects, id)
                .unwrap();
            assert_eq!(
                result(
                    &mut m,
                    &registry,
                    &format!("{interface}::GetDevice"),
                    &[pointer, output]
                ),
                0
            );
            assert_eq!(m.cpu.read_u32(output).unwrap(), device);
            assert_eq!(
                m.ext.get::<State>().com.references(&m.sys.objects, id),
                Some(before + 1)
            );
            release(&mut env(&mut m), device);
        }
    }

    // spec: GFX-RES-1
    #[test]
    fn package_a_device_parent_raster_and_software_registration() {
        let (mut m, registry, device) = fixture();
        let output = packet(&mut m, [9u32; 2]);
        let factory = super::super::device(&env(&mut m), device).unwrap().factory;
        let before = m
            .ext
            .get::<State>()
            .com
            .references(&m.sys.objects, factory)
            .unwrap();
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::GetDirect3D",
                &[device, output]
            ),
            0
        );
        let parent = m.cpu.read_u32(output).unwrap();
        assert_eq!(
            m.ext.get::<State>().com.references(&m.sys.objects, factory),
            Some(before + 1)
        );
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::GetNumberOfSwapChains",
                &[device]
            ),
            1
        );
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::GetRasterStatus",
                &[device, 0, output]
            ),
            0
        );
        assert_eq!(
            GuestPtr::<[u32; 2]>::new(output).read(&m.cpu).unwrap(),
            [0, 0]
        );
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::GetRasterStatus",
                &[device, 1, output]
            ),
            UNAVAILABLE
        );
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3D9::RegisterSoftwareDevice",
                &[parent, 0]
            ),
            UNAVAILABLE
        );
    }

    // spec: GFX-RES-1
    #[test]
    fn package_a_validate_device_matches_unsupported_stage_state() {
        let (mut m, registry, device) = fixture();
        let output = packet(&mut m, 99u32);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::ValidateDevice",
                &[device, output]
            ),
            0
        );
        assert_eq!(m.cpu.read_u32(output).unwrap(), 1);
        for (kind, value) in [(1, 2), (2, 4)] {
            assert_eq!(
                result(
                    &mut m,
                    &registry,
                    "IDirect3DDevice9::SetTextureStageState",
                    &[device, 1, kind, value]
                ),
                0
            );
        }
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::ValidateDevice",
                &[device, output]
            ),
            UNAVAILABLE
        );
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::SetTextureStageState",
                &[device, 1, 1, 1]
            ),
            0
        );
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::ValidateDevice",
                &[device, output]
            ),
            0
        );
    }

    // spec: GFX-RES-1
    #[test]
    fn package_a_gamma_ramp_roundtrips_without_gpu_work() {
        let (mut m, registry, device) = fixture();
        let output = packet(&mut m, [[0u16; 256]; 3]);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::GetGammaRamp",
                &[device, 0, output]
            ),
            0
        );
        let gamma = GuestPtr::<[[u16; 256]; 3]>::new(output)
            .read(&m.cpu)
            .unwrap();
        assert_eq!(gamma, [std::array::from_fn(|i| i as u16 * 257); 3]);
        let changed = [std::array::from_fn(|i| (255 - i) as u16 * 193); 3];
        let input = packet(&mut m, changed);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::SetGammaRamp",
                &[device, 0, 1, input]
            ),
            0
        );
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::GetGammaRamp",
                &[device, 0, output]
            ),
            0
        );
        assert_eq!(
            GuestPtr::<[[u16; 256]; 3]>::new(output)
                .read(&m.cpu)
                .unwrap(),
            changed
        );
        assert!(m.ext.get::<State>().pending.is_empty());
    }

    // spec: GFX-RES-1
    #[test]
    fn package_a_clip_status_roundtrips() {
        let (mut m, registry, device) = fixture();
        let output = packet(&mut m, [0u32; 2]);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::GetClipStatus",
                &[device, output]
            ),
            0
        );
        assert_eq!(
            GuestPtr::<[u32; 2]>::new(output).read(&m.cpu).unwrap(),
            [0, u32::MAX]
        );
        let input = packet(&mut m, [0x35u32, 0x10]);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::SetClipStatus",
                &[device, input]
            ),
            0
        );
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::GetClipStatus",
                &[device, output]
            ),
            0
        );
        assert_eq!(
            GuestPtr::<[u32; 2]>::new(output).read(&m.cpu).unwrap(),
            [0x35, 0x10]
        );
    }

    // spec: GFX-RES-1
    #[test]
    fn package_a_palette_entries_and_selection_roundtrip() {
        let (mut m, registry, device) = fixture();
        let output = packet(&mut m, [0u32; 256]);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::GetPaletteEntries",
                &[device, 3, output]
            ),
            0
        );
        assert_eq!(
            GuestPtr::<[u32; 256]>::new(output).read(&m.cpu).unwrap(),
            [u32::MAX; 256]
        );
        let entries = std::array::from_fn(|i| 0xff000000 | (i as u32 * 0x010101));
        let input = packet(&mut m, entries);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::SetPaletteEntries",
                &[device, 3, input]
            ),
            0
        );
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::SetCurrentTexturePalette",
                &[device, 3]
            ),
            0
        );
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::GetCurrentTexturePalette",
                &[device, output]
            ),
            0
        );
        assert_eq!(m.cpu.read_u32(output).unwrap(), 3);
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::GetPaletteEntries",
                &[device, 3, output]
            ),
            0
        );
        assert_eq!(
            GuestPtr::<[u32; 256]>::new(output).read(&m.cpu).unwrap(),
            entries
        );
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::SetCurrentTexturePalette",
                &[device, 65536]
            ),
            UNAVAILABLE
        );
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::CreateTexture",
                &[device, 1, 1, 1, 0, 41, 2, output, 0]
            ),
            UNAVAILABLE
        );
    }

    // spec: GFX-RES-1
    #[test]
    fn package_a_cursor_visibility_returns_previous_state_and_stores_position() {
        let (mut m, registry, device) = fixture();
        for (visible, previous) in [(1, 0), (7, 1), (0, 1), (0, 0)] {
            assert_eq!(
                result(
                    &mut m,
                    &registry,
                    "IDirect3DDevice9::ShowCursor",
                    &[device, visible]
                ),
                previous
            );
        }
        assert_eq!(
            result(
                &mut m,
                &registry,
                "IDirect3DDevice9::SetCursorPosition",
                &[device, (-8i32) as u32, 17, 1]
            ),
            0
        );
        assert_eq!(
            super::super::device(&env(&mut m), device)
                .unwrap()
                .misc
                .cursor_position,
            (-8, 17)
        );
    }
}
