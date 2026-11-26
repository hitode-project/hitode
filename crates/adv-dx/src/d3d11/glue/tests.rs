use super::*;
use crate::d3d11::{
    caps::{FeatureLevel, tests::fixture_report},
    desc::{Bind, Texture},
};
use adv_abi::{Conv, gfx_iids::*};
use adv_cpu::{
    JitCpu,
    backend::{Guest, asm::Asm},
};
use adv_machine::{Clock, RunResult, layout::PARK_ADDR};
use std::sync::Arc;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const DEVICE: u32 = DATA + 0x40;
const IID: u32 = DATA + 0x100;
const OUT: u32 = DATA + 0x120;
const LENGTH: u32 = DATA + 0x140;
const RESULT: u32 = DATA + 0x180;

fn machine() -> Machine<JitCpu> {
    let mut registry = Registry::default();
    register_children(&mut registry);
    let coverage = registry.coverage();
    assert_eq!(
        coverage.implement + coverage.state,
        FACES.iter().map(|(_, slots)| slots.len()).sum::<usize>()
    );
    assert_eq!((coverage.safe_fail, coverage.legacy), (0, 0));
    assert!(registry.signature_conflicts().is_empty());
    registry.add("fixture", "exit", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::ExitThread(0)
    });
    registry.add(
        "fixture",
        "create_resource_view",
        Conv::Stdcall { argc: 0 },
        |env| {
            let table = table(env);
            let report = Arc::new(fixture_report(FeatureLevel::L11_0));
            table
                .attach_device(&mut env.sys.objects, DEVICE, report.clone())
                .unwrap();
            let texture = Resource::Texture(
                Texture::new_2d(
                    D3D11_TEXTURE2D_DESC {
                        Width: 16,
                        Height: 16,
                        MipLevels: 1,
                        ArraySize: 1,
                        Format: 28,
                        SampleDesc: DXGI_SAMPLE_DESC {
                            Count: 1,
                            Quality: 0,
                        },
                        BindFlags: Bind::RENDER_TARGET,
                        ..Default::default()
                    },
                    false,
                    &report,
                )
                .unwrap(),
            );
            let view = View::new(ViewKind::RenderTarget, texture, None, &report).unwrap();
            let texture = attach_resource(env, DEVICE, texture).unwrap();
            let view = attach_view(env, texture, view).unwrap();
            env.cpu.write_u32(OUT, texture).unwrap();
            env.cpu.write_u32(OUT + 4, view).unwrap();
            Outcome::Return(0)
        },
    );
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    crate::test_support::install(&mut machine);
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    install_children(&mut machine).unwrap();
    machine
}

fn guest_call(machine: &mut Machine<JitCpu>, entry: u32, arguments: &[u32]) -> u32 {
    let exit = machine.resolve_import("fixture", "exit").unwrap();
    let mut code = Asm::new(CODE);
    for &argument in arguments.iter().rev() {
        code.push_imm(argument);
    }
    code.call(entry).mov_mem_eax(RESULT).call(exit).hlt();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("D3D11 fixture", CODE, &[], 65536).unwrap();
    assert!(matches!(machine.run(), RunResult::Exited(_)));
    machine.cpu.read_u32(RESULT).unwrap()
}

fn method(machine: &mut Machine<JitCpu>, object: u32, slot: u32, arguments: &[u32]) -> u32 {
    let vtable = machine.cpu.read_u32(object).unwrap();
    let entry = machine.cpu.read_u32(vtable + slot * 4).unwrap();
    let arguments = [std::slice::from_ref(&object), arguments].concat();
    guest_call(machine, entry, &arguments)
}

fn refs(machine: &mut Machine<JitCpu>, object: u32) -> u32 {
    method(machine, object, 1, &[]);
    method(machine, object, 2, &[])
}

// spec: GFX-RES-1
#[test]
fn resource_view_and_private_data_calls_use_guest_vtables() {
    let mut machine = machine();
    let create = machine
        .resolve_import("fixture", "create_resource_view")
        .unwrap();
    guest_call(&mut machine, create, &[]);
    let texture = machine.cpu.read_u32(OUT).unwrap();
    let view = machine.cpu.read_u32(OUT + 4).unwrap();
    let oracle = include_str!("../object/windows.tsv")
        .lines()
        .map(|line| line.split_once('\t').unwrap())
        .collect::<BTreeMap<_, _>>();
    let check = |case, result: String| assert_eq!(result, oracle[case], "{case}");

    machine.cpu.write(IID, &IID_ID3D11RESOURCE).unwrap();
    assert_eq!(method(&mut machine, texture, 0, &[IID, OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), texture);
    assert_eq!(method(&mut machine, texture, 2, &[]), 1);
    assert_eq!(method(&mut machine, texture, 7, &[OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), 3);
    assert_eq!(method(&mut machine, texture, 10, &[DATA + 0x300]), 0);
    let raw: D3D11_TEXTURE2D_DESC = GuestPtr::new(DATA + 0x300).read(&machine.cpu).unwrap();
    assert_eq!(
        (raw.Width, raw.Height, raw.BindFlags),
        (16, 16, Bind::RENDER_TARGET)
    );

    assert_eq!(method(&mut machine, view, 7, &[OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), texture);
    assert_eq!(method(&mut machine, texture, 2, &[]), 1);

    machine.cpu.write_u32(LENGTH, 99).unwrap();
    machine.cpu.write_u32(OUT, 0x12345678).unwrap();
    let status = method(&mut machine, view, 4, &[IID, LENGTH, OUT]);
    let (length, value) = (
        machine.cpu.read_u32(LENGTH).unwrap(),
        machine.cpu.read_u32(OUT).unwrap(),
    );
    check(
        "get-missing",
        format!("{status:08x}\t{length}\t{value:08x}"),
    );
    let status = method(&mut machine, view, 5, &[IID, 0, 0]);
    check("remove-missing", format!("{status:08x}"));

    let status = method(&mut machine, view, 6, &[IID, texture]);
    let count = refs(&mut machine, texture);
    check("set-interface", format!("{status:08x}\t{count}"));
    machine.cpu.write_u32(LENGTH, 1).unwrap();
    let status = method(&mut machine, view, 4, &[IID, LENGTH, OUT]);
    let (length, value) = (
        machine.cpu.read_u32(LENGTH).unwrap(),
        machine.cpu.read_u32(OUT).unwrap(),
    );
    let count = refs(&mut machine, texture);
    check(
        "small-interface",
        format!("{status:08x}\t{length}\t{value:08x}\t{count}"),
    );

    let status = method(&mut machine, view, 4, &[IID, LENGTH, 0]);
    let length = machine.cpu.read_u32(LENGTH).unwrap();
    let count = refs(&mut machine, texture);
    check("size-interface", format!("{status:08x}\t{length}\t{count}"));
    let status = method(&mut machine, view, 4, &[IID, LENGTH, OUT]);
    let length = machine.cpu.read_u32(LENGTH).unwrap();
    let identical = u32::from(machine.cpu.read_u32(OUT).unwrap() == texture);
    let count = refs(&mut machine, texture);
    check(
        "get-interface",
        format!("{status:08x}\t{length}\t{identical}\t{count}"),
    );
    assert_eq!(method(&mut machine, texture, 2, &[]), 2);

    let status = method(&mut machine, view, 6, &[IID, 0]);
    let count = refs(&mut machine, texture);
    check("null-interface", format!("{status:08x}\t{count}"));
    let status = method(&mut machine, view, 4, &[IID, LENGTH, OUT]);
    let length = machine.cpu.read_u32(LENGTH).unwrap();
    let null = u32::from(machine.cpu.read_u32(OUT).unwrap() == 0);
    check("get-after-null", format!("{status:08x}\t{length}\t{null}"));

    assert_eq!(method(&mut machine, texture, 2, &[]), 0);
    assert_eq!(method(&mut machine, view, 2, &[]), 0);
    let state = machine.ext.get::<State>();
    assert_eq!(
        state.table.references(&machine.sys.objects, DEVICE),
        Some(1)
    );
    assert!(state.table.get(&machine.sys.objects, texture).is_none());
}
