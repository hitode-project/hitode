use super::*;
use adv_abi::{ApiDiagnostics, ImplementationLevel};
use adv_cpu::backend::{Guest, Reg};
use adv_machine::{Clock, layout::teb_addr};

const STACK: u32 = 0x550000;
const METHOD: &str = "IDirect3DDevice9::SetCursorProperties";
const API: &str = "d3d9.dll!IDirect3DDevice9::SetCursorProperties";

fn prepare(machine: &mut Machine<adv_cpu::JitCpu>) {
    machine.cpu.map(STACK, 0x1000, Prot::RW).unwrap();
    machine.cpu.set_reg(Reg::Esp, STACK);
    machine.cpu.write_u32(STACK, 0x12345678).unwrap();
    machine.cpu.map(teb_addr(0), 0x1000, Prot::RW).unwrap();
    machine
        .cpu
        .write_u32(teb_addr(0) + 0x34, 0x76543210)
        .unwrap();
    machine.ext.get_mut::<State>().provider.display = |_| (64, 64, 60);
}

fn call(env: &mut Env<'_>, registry: &Registry, name: &str, args: &[u32]) -> u32 {
    for (i, &arg) in args.iter().enumerate() {
        env.cpu.write_u32(STACK + 4 + i as u32 * 4, arg).unwrap();
    }
    match (registry.get("d3d9.dll", name).unwrap().handler)(env) {
        Outcome::Return(result) => result,
        _ => panic!("{name} unexpectedly parked or called guest code"),
    }
}

#[test]
fn system_memory_cursor_request_fails_without_retaining_or_discarding_pixels() {
    let mut registry = Registry::default();
    super::super::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    assert_eq!(
        registry.get("d3d9.dll", METHOD).unwrap().conv,
        adv_machine::Conv::Stdcall { argc: 4 }
    );
    assert_eq!(properties_ABI.level, ImplementationLevel::SafeFail);

    let mut machine = super::super::tests::machine();
    prepare(&mut machine);
    let mut env = Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    let (device, owner, _, _) = super::super::tests::setup(&mut env);
    let output = alloc(&mut env, 8).unwrap();
    let before = env.sys.objects.object_count();
    assert_eq!(
        call(
            &mut env,
            &registry,
            "IDirect3DDevice9::CreateOffscreenPlainSurface",
            &[device, 32, 32, 21, 3, output, 0],
        ),
        0
    );
    let surface = env.cpu.read_u32(output).unwrap();
    let identity = com(&env)
        .identity::<Payload>(&env.sys.objects, surface)
        .unwrap();
    assert_eq!(env.sys.objects.object_count(), before + 1);
    assert_eq!(
        call(
            &mut env,
            &registry,
            "IDirect3DSurface9::LockRect",
            &[surface, output, 0, 0]
        ),
        0
    );
    assert_eq!(env.cpu.read_u32(output).unwrap(), 32 * 4);
    let pixels = env.cpu.read_u32(output + 4).unwrap();
    env.cpu.write_u32(pixels, 0xff332211).unwrap();
    assert_eq!(
        call(&mut env, &registry, METHOD, &[device, 0, 0, surface]),
        INVALID
    );
    assert_eq!(
        call(
            &mut env,
            &registry,
            "IDirect3DSurface9::UnlockRect",
            &[surface]
        ),
        0
    );

    let count = env.sys.objects.object_count();
    let aliases = env.sys.objects.alias_count(com(&env).namespace());
    let refs = env.sys.objects.strong_count(owner);
    for _ in 0..2 {
        assert_eq!(
            call(&mut env, &registry, METHOD, &[device, 0, 0, surface]),
            UNAVAILABLE
        );
    }
    assert_eq!(env.cpu.read_u32(pixels).unwrap(), 0xff332211);
    assert_eq!(env.sys.objects.object_count(), count);
    assert_eq!(env.sys.objects.alias_count(com(&env).namespace()), aliases);
    assert_eq!(env.sys.objects.strong_count(owner), refs);
    assert_eq!(com(&env).references(&env.sys.objects, identity), Some(1));
    assert_eq!(env.cpu.read_u32(teb_addr(0) + 0x34).unwrap(), 0x76543210);
    let diagnostics = env.ext.get::<ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert_eq!(diagnostics.warnings.len(), 1);
    assert_eq!(
        diagnostics.warnings.get(&(
            API.to_owned(),
            "D3DERR_NOTAVAILABLE (HRESULT=0x8876086a): custom cursor presentation is unavailable"
                .to_owned(),
        )),
        Some(&2)
    );
    assert_eq!(
        call(
            &mut env,
            &registry,
            "IDirect3DSurface9::Release",
            &[surface]
        ),
        0
    );
    assert!(com(&env).get_identity(&env.sys.objects, identity).is_none());
    assert_eq!(env.sys.objects.object_count(), before);
}

#[test]
fn cursor_validates_interface_owner_dimensions_format_and_hotspot() {
    let mut machine = super::super::tests::machine();
    prepare(&mut machine);
    let mut env = Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    let (pointer, owner, surface, id) = super::super::tests::setup(&mut env);
    let factory = device(&env, pointer).unwrap().factory;
    let factory_pointer = com(&env).canonical(&env.sys.objects, factory).unwrap();
    if let Some(Payload::Resource(r)) = com(&env).get_identity_mut(&mut env.sys.objects, id) {
        r.desc.Format = 21;
        r.desc.Width = 32;
        r.desc.Height = 32;
    }
    for invalid in [0, 0xdead0000, factory_pointer, surface] {
        assert_eq!(
            properties(
                &mut env,
                GuestPtr::new(invalid),
                0,
                0,
                GuestPtr::new(surface)
            ),
            INVALID
        );
    }
    for invalid in [0, 0xdead0000, pointer, factory_pointer] {
        assert_eq!(
            properties(
                &mut env,
                GuestPtr::new(pointer),
                0,
                0,
                GuestPtr::new(invalid)
            ),
            INVALID
        );
    }
    for (x, y) in [(32, 0), (0, 32), (u32::MAX, 0)] {
        assert_eq!(
            properties(
                &mut env,
                GuestPtr::new(pointer),
                x,
                y,
                GuestPtr::new(surface)
            ),
            INVALID
        );
    }
    for (width, height, format) in [
        (0, 32, 21),
        (31, 32, 21),
        (32, 33, 21),
        (64, 32, 21),
        (32, 64, 21),
        (32, 32, 22),
    ] {
        if let Some(Payload::Resource(r)) = com(&env).get_identity_mut(&mut env.sys.objects, id) {
            r.desc.Width = width;
            r.desc.Height = height;
            r.desc.Format = format;
        }
        assert_eq!(
            properties(
                &mut env,
                GuestPtr::new(pointer),
                0,
                0,
                GuestPtr::new(surface)
            ),
            INVALID
        );
    }
    for (kind, parent) in [
        (resources::Kind::Surface, factory),
        (resources::Kind::Texture, owner),
    ] {
        if let Some(Payload::Resource(r)) = com(&env).get_identity_mut(&mut env.sys.objects, id) {
            r.desc.Width = 32;
            r.desc.Height = 32;
            r.desc.Format = 21;
            r.kind = kind;
            r.device = parent;
        }
        assert_eq!(
            properties(
                &mut env,
                GuestPtr::new(pointer),
                0,
                0,
                GuestPtr::new(surface)
            ),
            INVALID
        );
    }
    assert!(env.ext.try_get::<ApiDiagnostics>().is_none());
    assert_eq!(env.cpu.read_u32(teb_addr(0) + 0x34).unwrap(), 0x76543210);

    let mut absent = Machine::new(
        adv_cpu::JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap(),
        Registry::default(),
        Clock::virtual_time(),
    )
    .unwrap();
    let mut env = Env {
        context: 0,
        cpu: &mut absent.cpu,
        sys: &mut absent.sys,
        ext: &mut absent.ext,
        thunk: 0,
    };
    assert_eq!(
        properties(
            &mut env,
            GuestPtr::new(pointer),
            0,
            0,
            GuestPtr::new(surface)
        ),
        INVALID
    );
}
