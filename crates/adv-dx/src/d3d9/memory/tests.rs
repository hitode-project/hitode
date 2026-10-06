use super::*;
use adv_abi::{ApiDiagnostics, ImplementationLevel};
use adv_cpu::backend::{Guest, Reg};
use adv_machine::{Clock, layout::teb_addr};

const STACK: u32 = 0x550000;
const API: &str = "d3d9.dll!IDirect3DDevice9::GetAvailableTextureMem";

fn prepare(machine: &mut Machine<adv_cpu::JitCpu>) {
    machine.cpu.map(STACK, 0x1000, Prot::RW).unwrap();
    machine.cpu.set_reg(Reg::Esp, STACK);
    machine.cpu.write_u32(STACK, 0x12345678).unwrap();
    machine.cpu.map(teb_addr(0), 0x1000, Prot::RW).unwrap();
    machine
        .cpu
        .write_u32(teb_addr(0) + 0x34, 0x76543210)
        .unwrap();
}

#[test]
fn unknown_budget_uses_typed_uint_adapter_without_changing_device_or_last_error() {
    let mut registry = Registry::default();
    register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    let method = registry
        .get("d3d9.dll", "IDirect3DDevice9::GetAvailableTextureMem")
        .unwrap();
    assert_eq!(method.conv, adv_machine::Conv::Stdcall { argc: 1 });
    assert_eq!(available_ABI.level, ImplementationLevel::SafeFail);

    let mut machine = super::super::tests::machine();
    prepare(&mut machine);
    let mut env = Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    let (pointer, owner, _, _) = super::super::tests::setup(&mut env);
    let count = env.sys.objects.object_count();
    let aliases = env.sys.objects.alias_count(com(&env).namespace());
    let refs = env.sys.objects.strong_count(owner);
    env.cpu.write_u32(STACK + 4, pointer).unwrap();
    for _ in 0..2 {
        assert!(matches!((method.handler)(&mut env), Outcome::Return(0)));
    }
    assert_eq!(env.sys.objects.object_count(), count);
    assert_eq!(env.sys.objects.alias_count(com(&env).namespace()), aliases);
    assert_eq!(env.sys.objects.strong_count(owner), refs);
    assert_eq!(env.cpu.read_u32(teb_addr(0) + 0x34).unwrap(), 0x76543210);
    let diagnostics = env.ext.get::<ApiDiagnostics>();
    assert_eq!(diagnostics.stub_calls, 0);
    assert_eq!(diagnostics.warnings.len(), 1);
    assert_eq!(
        diagnostics.warnings.get(&(
            API.to_owned(),
            "texture-memory budget is unavailable; conservative estimate is 0 bytes".to_owned()
        )),
        Some(&2)
    );
}

#[test]
fn wrong_interface_and_absent_front_return_zero_with_an_explicit_diagnostic() {
    let mut machine = super::super::tests::machine();
    prepare(&mut machine);
    let mut env = Env {
        context: 0,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    let (pointer, owner, resource, _) = super::super::tests::setup(&mut env);
    let factory = com(&env)
        .canonical(&env.sys.objects, device(&env, pointer).unwrap().factory)
        .unwrap();
    let count = env.sys.objects.object_count();
    let refs = env.sys.objects.strong_count(owner);
    for invalid in [0, 0xdead0000, factory, resource] {
        assert_eq!(available(&mut env, GuestPtr::new(invalid)), 0);
    }
    assert_eq!(env.sys.objects.object_count(), count);
    assert_eq!(env.sys.objects.strong_count(owner), refs);
    assert_eq!(
        env.ext
            .get::<ApiDiagnostics>()
            .warnings
            .values()
            .sum::<u64>(),
        4
    );
    assert_eq!(env.cpu.read_u32(teb_addr(0) + 0x34).unwrap(), 0x76543210);

    let mut absent = Machine::new(
        adv_cpu::JitCpu::new(adv_machine::layout::PARK_ADDR, false).unwrap(),
        Registry::default(),
        Clock::virtual_time(),
    )
    .unwrap();
    prepare(&mut absent);
    let mut env = Env {
        context: 0,
        cpu: &mut absent.cpu,
        sys: &mut absent.sys,
        ext: &mut absent.ext,
        thunk: 0,
    };
    assert_eq!(available(&mut env, GuestPtr::new(pointer)), 0);
    assert_eq!(
        env.ext
            .get::<ApiDiagnostics>()
            .warnings
            .values()
            .sum::<u64>(),
        1
    );
}
