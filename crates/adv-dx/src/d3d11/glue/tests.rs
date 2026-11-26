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
        coverage.implement + coverage.state + coverage.safe_fail,
        140 + FACES
            .iter()
            .chain(d3d10::FACES)
            .map(|(_, slots)| slots.len())
            .sum::<usize>()
    );
    assert_eq!(coverage.legacy, 0);
    let excluded: Vec<_> = FACES
        .iter()
        .flat_map(|(_, slots)| slots.iter())
        .filter_map(|&(name, _, level)| (level == ImplementationLevel::SafeFail).then_some(name))
        .collect();
    assert_eq!(
        excluded,
        [
            "ID3D11Device1::CreateGeometryShader",
            "ID3D11Device1::CreateGeometryShaderWithStreamOutput",
            "ID3D11Device1::CreateHullShader",
            "ID3D11Device1::CreateDomainShader",
            "ID3D11Device1::CreateClassLinkage",
        ]
    );
    assert!(registry.signature_conflicts().is_empty());
    crate::dxgi::register(&mut registry);
    #[cfg(feature = "gpu")]
    {
        crate::ddraw::register(&mut registry);
        registry.add("fixture", "alt_enter", Conv::Stdcall { argc: 1 }, |env| {
            Outcome::Return(u32::from(crate::dxgi::alt_enter(env, env.arg(0))))
        });
    }
    registry.add("fixture", "exit", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::ExitThread(0)
    });
    registry.add("fixture", "layer_size", Conv::Stdcall { argc: 3 }, |env| {
        assert_eq!(env.arg(0), 0xaaaa0001);
        assert_eq!(env.arg(2), 40);
        let shifted: [u32; 10] = GuestPtr::new(env.arg(1)).read(env.cpu).unwrap();
        assert_eq!(
            &shifted[..4],
            &[0xc0de0000, 0xc0de0001, DATA + 0x904, DATA + 0x914]
        );
        Outcome::Return(0x1234)
    });
    registry.add(
        "fixture",
        "layer_create",
        Conv::Stdcall { argc: 6 },
        |env| {
            assert_eq!(env.arg(0), 0xaaaa0001);
            assert_eq!(env.arg(2), 40);
            assert_eq!(env.arg(3), 0x77);
            assert_eq!(env.arg(4), IID);
            let shifted: [u32; 10] = GuestPtr::new(env.arg(1)).read(env.cpu).unwrap();
            assert_eq!(
                &shifted[..4],
                &[0xc0de0000, 0xc0de0001, DATA + 0x904, DATA + 0x914]
            );
            env.cpu.write_u32(env.arg(5), 0x5678).unwrap();
            Outcome::Return(0x123)
        },
    );
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
    crate::dxgi::install(
        &mut machine,
        crate::dxgi::Provider {
            adapter: Default::default(),
            display: crate::dxgi::shared_display,
            window_exists: |_, _, window| window == 1,
            next_vblank: |_| None,
            devices: None,
        },
    )
    .unwrap();
    machine.ext.get_mut::<State>().devices = device::CreateDevices {
        reports: FeatureLevel::ALL.map(|level| Some(Arc::new(fixture_report(level)))),
        max_buffer_size: 1 << 28,
    };
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

fn check_vtable(machine: &mut Machine<JitCpu>, object: u32, dll: &str, interface: &str) {
    let vtable = machine.cpu.read_u32(object).unwrap();
    for signature in adv_abi::signature::com_signatures()
        .iter()
        .filter(|signature| signature.module == interface)
    {
        let name = format!("{interface}::{}", signature.name);
        let index = u32::from(signature.com_slot.unwrap());
        let published = machine.resolve_import(dll, &name).unwrap();
        assert_eq!(
            machine.cpu.read_u32(vtable + 4 * index).unwrap(),
            published,
            "{name}"
        );
    }
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

fn create(machine: &mut Machine<JitCpu>, driver: u32, adapter: u32, levels: &[u32]) -> (u32, u32) {
    let levels_address = DATA + 0x600;
    machine
        .cpu
        .write(levels_address, bytemuck::cast_slice(levels))
        .unwrap();
    let call = machine.resolve_import(DLL, "D3D11CreateDevice").unwrap();
    assert_eq!(
        guest_call(
            machine,
            call,
            &[
                adapter,
                driver,
                0,
                0,
                levels_address,
                levels.len() as u32,
                7,
                OUT,
                LEVEL,
                CONTEXT
            ]
        ),
        0
    );
    (
        machine.cpu.read_u32(OUT).unwrap(),
        machine.cpu.read_u32(CONTEXT).unwrap(),
    )
}

const LEVEL: u32 = OUT + 16;
const CONTEXT: u32 = OUT + 20;

// spec: GFX-CAP-4 GFX-DXGI-1
#[test]
fn device_entry_negotiates_levels_and_retains_its_immediate_context() {
    let mut machine = machine();
    let (device, context) = create(&mut machine, 5, 0, &[0x9100, 0xb000]);
    check_vtable(&mut machine, device, DLL, "ID3D11Device1");
    check_vtable(&mut machine, context, DLL, "ID3D11DeviceContext1");
    assert_eq!(machine.cpu.read_u32(LEVEL).unwrap(), 0x9100);
    assert_eq!(method(&mut machine, device, 37, &[]), 0x9100);
    assert_eq!(refs(&mut machine, device), 2);
    assert_eq!(method(&mut machine, context, 112, &[]), 0);
    assert_eq!(method(&mut machine, context, 2, &[]), 0);
    assert_eq!(refs(&mut machine, device), 1);
    assert_eq!(method(&mut machine, device, 40, &[CONTEXT]), 0);
    assert_eq!(machine.cpu.read_u32(CONTEXT).unwrap(), context);
    assert_eq!(refs(&mut machine, device), 2);
    machine.cpu.write(IID, &IID_IDXGIDEVICE).unwrap();
    assert_eq!(method(&mut machine, device, 0, &[IID, OUT]), 0);
    let dxgi = machine.cpu.read_u32(OUT).unwrap();
    check_vtable(&mut machine, dxgi, DLL, "IDXGIDevice1");
    assert_ne!(device, dxgi);
    machine.cpu.write(IID, &adv_abi::com::IUNKNOWN).unwrap();
    assert_eq!(method(&mut machine, dxgi, 0, &[IID, OUT]), 0);
    assert_eq!(machine.cpu.read_u32(OUT).unwrap(), device);
    assert_eq!(method(&mut machine, dxgi, 7, &[OUT]), 0);
    let adapter = machine.cpu.read_u32(OUT).unwrap();
    let desc = DATA + 0x650;
    assert_eq!(method(&mut machine, adapter, 8, &[desc]), 0);
    let desc: adv_abi::types::DXGI_ADAPTER_DESC = GuestPtr::new(desc).read(&machine.cpu).unwrap();
    assert_eq!((desc.VendorId, desc.DeviceId), (0x1414, 0x008c));
    assert_eq!(method(&mut machine, adapter, 2, &[]), 1);
    assert_eq!(method(&mut machine, dxgi, 2, &[]), 3);
    assert_eq!(method(&mut machine, device, 2, &[]), 2);
    assert_eq!(method(&mut machine, context, 2, &[]), 0);
    assert_eq!(method(&mut machine, device, 2, &[]), 0);
}

#[cfg(feature = "gpu")]
mod pixels;

mod interop;
