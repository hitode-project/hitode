use super::*;
use crate::dxgi::caps::{AdapterInfo, Display, PhysicalMode};
use adv_abi::Conv;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, asm::Asm},
};
use adv_machine::{Clock, Ext, RunResult, layout::PARK_ADDR};
use std::num::NonZeroU32;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const RESULT: u32 = DATA + 0xff0;
const IID: u32 = DATA + 0x200;
const OBJECT: u32 = DATA + 0x220;

struct Aware(bool);

fn display(ext: &Ext, _: u32) -> Option<Display> {
    Some(Display {
        name: r"\\.\DISPLAY1".into(),
        monitor: 7,
        physical_rect: [0, 0, 2560, 1440],
        dpi: NonZeroU32::new(120).unwrap(),
        dpi_aware: ext.get::<Aware>().0,
        modes: [
            ((1280, 720), 60, 0),
            ((1920, 1080), 60, 0),
            ((2560, 1440), 60, 1),
            ((2560, 1440), 120, 2),
        ]
        .into_iter()
        .map(|(size, refresh, scaling)| PhysicalMode {
            size,
            refresh: (refresh, NonZeroU32::MIN),
            scaling,
            scanline: 1,
            stereo: false,
        })
        .collect(),
    })
}

fn machine() -> Machine<JitCpu> {
    let mut registry = Registry::default();
    register(&mut registry);
    registry.add("fixture", "exit", Conv::Stdcall { argc: 0 }, |_| {
        Outcome::ExitThread(0)
    });
    registry.add(
        "fixture",
        "device_factory",
        Conv::Stdcall { argc: 1 },
        |env| {
            interface_out(env, env.arg(0), |env| {
                ops::create_factory(env, FactoryOrigin::Device, IID_IDXGIFACTORY7)
            })
        },
    );
    assert!(registry.signature_conflicts().is_empty());
    let mut m = Machine::new(
        JitCpu::new(PARK_ADDR, false).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    crate::test_support::install(&mut m);
    m.cpu.map(CODE, 4096, Prot::RX).unwrap();
    m.cpu.map(DATA, 4096, Prot::RW).unwrap();
    m.ext.insert(Aware(false));
    install(
        &mut m,
        Provider {
            adapter: AdapterInfo {
                name: "Fixture GPU".into(),
                vendor: 0x1234,
                device: 0x5678,
                driver_version: 0x1234_5678,
                ..AdapterInfo::default()
            },
            display,
            window_exists: |_, _, handle| handle == 7,
            next_vblank: |_| None,
            devices: None,
        },
    )
    .unwrap();
    m
}

fn vtable(m: &Machine<JitCpu>, object: u32, slot: u32) -> u32 {
    let pointer = m.cpu.read_u32(object).unwrap();
    m.cpu.read_u32(pointer + slot * 4).unwrap()
}

fn guest_call(m: &mut Machine<JitCpu>, entry: u32, arguments: &[u32]) -> u32 {
    let exit = m.resolve_import("fixture", "exit").unwrap();
    let mut code = Asm::new(CODE);
    for &argument in arguments.iter().rev() {
        code.push_imm(argument);
    }
    code.call(entry).mov_mem_eax(RESULT).call(exit).hlt();
    m.cpu.write(CODE, &code.bytes).unwrap();
    m.spawn("DXGI fixture caller", CODE, &[], 65536).unwrap();
    assert!(matches!(m.run(), RunResult::Exited(_)));
    m.cpu.read_u32(RESULT).unwrap()
}

fn export(m: &mut Machine<JitCpu>, name: &str, iid: Iid) -> (u32, u32) {
    m.cpu.write(IID, &iid).unwrap();
    let entry = m.resolve_import(DLL, name).unwrap();
    let hr = guest_call(m, entry, &[IID, OBJECT]);
    (hr, m.cpu.read_u32(OBJECT).unwrap())
}

fn method(m: &mut Machine<JitCpu>, object: u32, slot: u32, arguments: &[u32]) -> u32 {
    let entry = vtable(m, object, slot);
    let args = [std::slice::from_ref(&object), arguments].concat();
    guest_call(m, entry, &args)
}

fn qi(m: &mut Machine<JitCpu>, object: u32, iid: Iid) -> (u32, u32) {
    m.cpu.write(IID, &iid).unwrap();
    let hr = method(m, object, 0, &[IID, OBJECT]);
    (hr, m.cpu.read_u32(OBJECT).unwrap())
}

fn enumerate(m: &mut Machine<JitCpu>, object: u32, slot: u32, index: u32) -> u32 {
    assert_eq!(method(m, object, slot, &[index, OBJECT]), 0);
    m.cpu.read_u32(OBJECT).unwrap()
}

// spec: GFX-DXGI-1
#[test]
fn factory_profiles_and_adapter_identity() {
    let mut m = machine();
    let (hr, factory) = export(&mut m, "CreateDXGIFactory", IID_IDXGIFACTORY);
    assert_eq!(hr, 0);
    assert_eq!(
        qi(&mut m, factory, IID_IDXGIFACTORY1),
        (Error::NoInterface.hresult(), 0)
    );
    assert_eq!(qi(&mut m, factory, IID_IDXGIFACTORY2), (0, factory));
    assert_eq!(method(&mut m, factory, 2, &[]), 1);
    assert_eq!(
        export(&mut m, "CreateDXGIFactory", IID_IDXGIFACTORY1),
        (Error::NoInterface.hresult(), 0)
    );
    assert_eq!(method(&mut m, factory, 2, &[]), 0);

    let (hr, factory) = export(&mut m, "CreateDXGIFactory1", IID_IDXGIFACTORY1);
    assert_eq!(hr, 0);
    assert_eq!(qi(&mut m, factory, IID_IDXGIFACTORY2), (0, factory));
    assert_eq!(method(&mut m, factory, 2, &[]), 1);
    let gpu = enumerate(&mut m, factory, 12, 0);
    let software = enumerate(&mut m, factory, 12, 1);
    assert_eq!(
        method(&mut m, factory, 12, &[2, OBJECT]),
        Error::NotFound.hresult()
    );
    assert_eq!(m.cpu.read_u32(OBJECT).unwrap(), 0);
    assert_eq!(
        method(&mut m, software, 7, &[0, OBJECT]),
        Error::NotFound.hresult()
    );
    for (adapter, vendor, device, flags) in
        [(gpu, 0x1234, 0x5678, 0), (software, 0x1414, 0x008c, 2)]
    {
        assert_eq!(method(&mut m, adapter, 10, &[DATA]), 0);
        let desc = GuestPtr::<DXGI_ADAPTER_DESC1>::new(DATA)
            .read(&m.cpu)
            .unwrap();
        assert_eq!(
            (desc.VendorId, desc.DeviceId, desc.Flags),
            (vendor, device, flags)
        );
        let name = String::from_utf16_lossy(&desc.Description);
        assert!(name.starts_with(if flags == 2 {
            "Microsoft Basic Render Driver"
        } else {
            "Fixture GPU"
        }));
        assert_eq!(desc.DedicatedVideoMemory, 0);
        for (iid, expected) in [
            (IID_IDXGIDEVICE, 0),
            (IID_ID3D10DEVICE, 0),
            (IID_ID3D11DEVICE, Error::Unsupported.hresult()),
        ] {
            m.cpu.write(IID, &iid).unwrap();
            assert_eq!(method(&mut m, adapter, 9, &[IID, DATA]), expected);
        }
    }
    m.cpu.write(IID, &IID_IDXGIFACTORY1).unwrap();
    assert_eq!(method(&mut m, software, 6, &[IID, OBJECT]), 0);
    assert_eq!(m.cpu.read_u32(OBJECT).unwrap(), factory);
}

fn entries(m: &mut Machine<JitCpu>, object: u32, interfaces: &[(Iid, &str)], slots: &[Slot]) {
    for &(iid, interface) in interfaces {
        let (hr, pointer) = qi(m, object, iid);
        assert_eq!((hr, pointer), (0, object), "{interface}");
        for method in signature::com_signatures()
            .iter()
            .filter(|method| method.module == interface)
        {
            let slot = u32::from(method.com_slot.unwrap());
            let &(name, _, _) = &slots[slot as usize];
            assert!(name.ends_with(method.name));
            let registry_entry = m.resolve_import(DLL, name).unwrap();
            assert_eq!(
                vtable(m, pointer, slot),
                registry_entry,
                "{interface}::{}",
                method.name
            );
        }
        method(m, pointer, 2, &[]);
    }
}

// spec: GFX-DXGI-5 PERS-EXPORT-2
#[test]
fn every_queried_interface_uses_covered_guest_thunks() {
    let mut registry = Registry::default();
    register(&mut registry);
    assert!(
        !registry
            .classify(DLL, "CreateDXGIFactory2")
            .is_some_and(adv_abi::ExportClass::discoverable)
    );
    assert!(!registry.dynamic_available(DLL, "CreateDXGIFactory2"));
    registry.set_expose_fixed_exports(true);
    assert!(!registry.dynamic_available(DLL, "CreateDXGIFactory2"));
    let count = registry.coverage();
    assert_eq!(
        count.implement + count.state + count.safe_fail,
        FACTORY.len() + ADAPTER.len() + OUTPUT.len() + swap_chain::SLOTS.len() + 50
    );
    assert_eq!(count.safe_fail, 1);
    assert_eq!(count.legacy, 0);
    for &(name, _, _) in FACTORY
        .iter()
        .chain(ADAPTER)
        .chain(OUTPUT)
        .chain(swap_chain::SLOTS)
    {
        let level = registry.level(DLL, name);
        assert!(
            matches!(
                level,
                Some(ImplementationLevel::Implement | ImplementationLevel::State)
            ) || (name == "IDXGIFactory7::CreateSoftwareAdapter"
                && level == Some(ImplementationLevel::SafeFail))
        );
    }

    let mut m = machine();
    let entry = m.resolve_import("fixture", "device_factory").unwrap();
    assert_eq!(guest_call(&mut m, entry, &[OBJECT]), 0);
    let factory = m.cpu.read_u32(OBJECT).unwrap();
    entries(
        &mut m,
        factory,
        &[
            (adv_abi::com::IUNKNOWN, "IUnknown"),
            (IID_IDXGIOBJECT, "IDXGIObject"),
            (IID_IDXGIFACTORY, "IDXGIFactory"),
            (IID_IDXGIFACTORY1, "IDXGIFactory1"),
            (IID_IDXGIFACTORY2, "IDXGIFactory2"),
            (IID_IDXGIFACTORY3, "IDXGIFactory3"),
            (IID_IDXGIFACTORY4, "IDXGIFactory4"),
            (IID_IDXGIFACTORY5, "IDXGIFactory5"),
            (IID_IDXGIFACTORY6, "IDXGIFactory6"),
            (IID_IDXGIFACTORY7, "IDXGIFactory7"),
        ],
        FACTORY,
    );
    let adapter = enumerate(&mut m, factory, 12, 0);
    entries(
        &mut m,
        adapter,
        &[
            (IID_IDXGIADAPTER, "IDXGIAdapter"),
            (IID_IDXGIADAPTER1, "IDXGIAdapter1"),
            (IID_IDXGIADAPTER2, "IDXGIAdapter2"),
            (IID_IDXGIADAPTER3, "IDXGIAdapter3"),
            (IID_IDXGIADAPTER4, "IDXGIAdapter4"),
        ],
        ADAPTER,
    );
    let output = enumerate(&mut m, adapter, 7, 0);
    entries(
        &mut m,
        output,
        &[
            (IID_IDXGIOUTPUT, "IDXGIOutput"),
            (IID_IDXGIOUTPUT1, "IDXGIOutput1"),
            (IID_IDXGIOUTPUT2, "IDXGIOutput2"),
            (IID_IDXGIOUTPUT3, "IDXGIOutput3"),
            (IID_IDXGIOUTPUT4, "IDXGIOutput4"),
            (IID_IDXGIOUTPUT5, "IDXGIOutput5"),
            (IID_IDXGIOUTPUT6, "IDXGIOutput6"),
        ],
        OUTPUT,
    );
    assert_eq!(method(&mut m, factory, 2, &[]), 1);
    assert_eq!(method(&mut m, adapter, 2, &[]), 1);
    assert_eq!(method(&mut m, output, 2, &[]), 0);
}

// spec: GFX-DXGI-4
#[test]
fn output_virtualizes_coordinates_and_keeps_physical_modes() {
    let mut m = machine();
    let (_, factory) = export(&mut m, "CreateDXGIFactory1", IID_IDXGIFACTORY1);
    let adapter = enumerate(&mut m, factory, 12, 0);
    let output = enumerate(&mut m, adapter, 7, 0);
    for (aware, width, height) in [(false, 2048, 1152), (true, 2560, 1440)] {
        m.ext.get_mut::<Aware>().0 = aware;
        assert_eq!(method(&mut m, output, 7, &[DATA]), 0);
        let desc = GuestPtr::<DXGI_OUTPUT_DESC>::new(DATA)
            .read(&m.cpu)
            .unwrap();
        assert_eq!(
            (
                desc.DesktopCoordinates.right,
                desc.DesktopCoordinates.bottom,
                desc.Rotation
            ),
            (width, height, 1)
        );
        assert_eq!(method(&mut m, output, 27, &[DATA]), 0);
        let desc1 = GuestPtr::<DXGI_OUTPUT_DESC1>::new(DATA)
            .read(&m.cpu)
            .unwrap();
        assert_eq!(
            (
                desc1.DesktopCoordinates.right,
                desc1.DesktopCoordinates.bottom
            ),
            (width, height)
        );

        for (format, flags) in [(28, 0), (87, 0), (28, 2)] {
            assert_eq!(method(&mut m, output, 8, &[format, flags, OBJECT, 0]), 0);
            assert_eq!(m.cpu.read_u32(OBJECT).unwrap(), 4);
            m.cpu.write_u32(OBJECT, 3).unwrap();
            assert_eq!(
                method(&mut m, output, 8, &[format, flags, OBJECT, DATA]),
                Error::MoreData.hresult()
            );
            assert_eq!(m.cpu.read_u32(OBJECT).unwrap(), 4);
            assert_eq!(method(&mut m, output, 8, &[format, flags, OBJECT, DATA]), 0);
            let largest = GuestPtr::<DXGI_MODE_DESC>::new(DATA)
                .offset(3)
                .unwrap()
                .read(&m.cpu)
                .unwrap();
            assert_eq!(
                (
                    largest.Width,
                    largest.Height,
                    largest.RefreshRate.Numerator,
                    largest.Format
                ),
                (2560, 1440, 120, format as i32)
            );
        }
        let request = DXGI_MODE_DESC {
            Width: 640,
            Height: 480,
            Format: 28,
            ..Default::default()
        };
        GuestPtr::new(DATA).write(&mut m.cpu, request).unwrap();
        assert_eq!(method(&mut m, output, 9, &[DATA, DATA + 32, 0]), 0);
        let matched = GuestPtr::<DXGI_MODE_DESC>::new(DATA + 32)
            .read(&m.cpu)
            .unwrap();
        assert_eq!((matched.Width, matched.Height), (1280, 720));
    }
}

#[test]
fn private_interface_values_hold_and_transfer_references() {
    let mut m = machine();
    let (_, factory) = export(&mut m, "CreateDXGIFactory1", IID_IDXGIFACTORY1);
    let adapter = enumerate(&mut m, factory, 12, 0);
    let key = [0xfd; 16];
    m.cpu.write(IID, &key).unwrap();
    assert_eq!(method(&mut m, factory, 4, &[IID, adapter]), 0);
    assert_eq!(method(&mut m, adapter, 2, &[]), 1);

    m.cpu.write_u32(OBJECT, 2).unwrap();
    assert_eq!(
        method(&mut m, factory, 5, &[IID, OBJECT, DATA]),
        Error::MoreData.hresult()
    );
    assert_eq!(m.cpu.read_u32(OBJECT).unwrap(), 4);
    assert_eq!(method(&mut m, factory, 5, &[IID, OBJECT, DATA]), 0);
    assert_eq!(m.cpu.read_u32(DATA).unwrap(), adapter);
    assert_eq!(method(&mut m, adapter, 2, &[]), 1);
    assert_eq!(method(&mut m, factory, 4, &[IID, 0]), 0);
    assert_eq!(
        method(&mut m, factory, 5, &[IID, OBJECT, DATA]),
        Error::NotFound.hresult()
    );
    assert_eq!(method(&mut m, factory, 2, &[]), 0);
}

#[test]
fn device_methods_require_a_device_and_clear_failed_creations() {
    let mut m = machine();
    let (_, factory) = export(&mut m, "CreateDXGIFactory1", IID_IDXGIFACTORY1);
    let adapter = enumerate(&mut m, factory, 12, 0);
    let output = enumerate(&mut m, adapter, 7, 0);
    GuestPtr::new(DATA)
        .write(&mut m.cpu, DXGI_SWAP_CHAIN_DESC::default())
        .unwrap();
    assert_eq!(
        method(&mut m, factory, 10, &[factory, DATA, OBJECT]),
        Error::InvalidArgument.hresult()
    );
    assert_eq!(m.cpu.read_u32(OBJECT).unwrap(), 0);
    assert_eq!(
        method(&mut m, output, 22, &[factory, OBJECT]),
        Error::InvalidArgument.hresult()
    );
    assert_eq!(m.cpu.read_u32(OBJECT).unwrap(), 0);
    assert_eq!(method(&mut m, factory, 8, &[7, 2]), 0);
    assert_eq!(method(&mut m, factory, 9, &[OBJECT]), 0);
    assert_eq!(m.cpu.read_u32(OBJECT).unwrap(), 7);
}
