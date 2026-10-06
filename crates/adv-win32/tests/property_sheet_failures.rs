//! Callable bootstrap exports must report unavailable UI without page handles,
//! callback execution or a fabricated modal Cancel result.
use adv_abi::Registry;
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const CALLBACK: u32 = CODE + 0x800;
const DATA: u32 = 0x410000;

fn invoke(
    machine: &mut Machine<JitCpu>,
    code: &mut Asm,
    api: &str,
    argument: Option<u32>,
    output: u32,
) {
    let (module, name) = api.split_once('!').unwrap();
    if let Some(argument) = argument {
        code.push_imm(argument);
    }
    code.call(machine.resolve_import(module, name).unwrap())
        .mov_mem_eax(output);
}

#[test]
fn property_sheet_exports_fail_with_typed_abi_and_preserve_guest_ownership() {
    for jit in [false, true] {
        let mut registry = Registry::default();
        adv_win32::register(&mut registry);
        let mut machine = Machine::new(
            JitCpu::new(PARK_ADDR, jit).unwrap(),
            registry,
            Clock::virtual_time(),
        )
        .unwrap();
        Win32::install(
            &mut machine,
            Win32Config::new(
                "/nonexistent/property-sheet",
                "property-sheet.exe",
                "/nonexistent/property-sheet-data",
            ),
        )
        .unwrap();
        machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
        machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
        let registry = (machine.sys.thunks.resolver() as &dyn std::any::Any)
            .downcast_ref::<Registry>()
            .unwrap();
        assert!(registry.signature_conflicts().is_empty());
        let before = machine.sys.objects.object_count();
        let page = DATA;
        let header = DATA + 0x100;
        let modeless = DATA + 0x200;
        let malformed = DATA + 0x300;
        let too_many = DATA + 0x400;
        let callback_called = DATA + 0x500;
        for (address, size) in [
            (page, 56),
            (header, 52),
            (modeless, 40),
            (malformed, 4),
            (too_many, 52),
        ] {
            machine.cpu.write_u32(address, size).unwrap();
        }
        machine.cpu.write_u32(page + 24, CALLBACK).unwrap();
        machine.cpu.write_u32(page + 32, CALLBACK).unwrap();
        for address in [header, modeless, too_many] {
            machine.cpu.write_u32(address + 24, 1).unwrap();
            machine.cpu.write_u32(address + 36, CALLBACK).unwrap();
        }
        machine.cpu.write_u32(modeless + 4, 0x400).unwrap();
        machine.cpu.write_u32(too_many + 24, 100).unwrap();
        let mut callback = Asm::new(CALLBACK);
        callback.mov_mem_imm(callback_called, 1);
        callback.bytes.extend_from_slice(&[0x31, 0xc0]);
        callback.ret_n(12);
        machine.cpu.write(CALLBACK, &callback.bytes).unwrap();
        let cases = [
            ("comctl32.dll!CreatePropertySheetPageA", page, 0, 120),
            ("comctl32.dll!CreatePropertySheetPageA", 0, 0, 87),
            ("comctl32.dll!CreatePropertySheetPageA", malformed, 0, 87),
            (
                "comctl32.dll!CreatePropertySheetPageA",
                DATA + 4096 - 4,
                0,
                998,
            ),
            ("comctl32.dll!PropertySheetA", header, u32::MAX, 120),
            ("comctl32.dll!PropertySheetA", modeless, 0, 120),
            ("comctl32.dll!PropertySheetA", malformed, u32::MAX, 87),
            ("comctl32.dll!PropertySheetA", 0xdead0000, u32::MAX, 998),
            ("comctl32.dll!PropertySheetA", too_many, u32::MAX, 0),
        ];
        machine.cpu.write_u32(DATA + 4096 - 4, 56).unwrap();
        let mut a = Asm::new(CODE);
        for (index, (api, argument, _, _)) in cases.iter().enumerate() {
            let output = DATA + 0x600 + index as u32 * 8;
            invoke(&mut machine, &mut a, api, Some(*argument), output);
            invoke(
                &mut machine,
                &mut a,
                "kernel32.dll!GetLastError",
                None,
                output + 4,
            );
        }
        a.bytes.extend_from_slice(&[0x31, 0xc0]);
        a.ret();
        machine.cpu.write(CODE, &a.bytes).unwrap();
        machine
            .spawn("property sheet failures", CODE, &[], 0x10000)
            .unwrap();
        let result = machine.run_virtual(1_000_000);
        assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
        for (index, (_, _, value, error)) in cases.iter().enumerate() {
            let output = DATA + 0x600 + index as u32 * 8;
            assert_eq!(machine.cpu.read_u32(output).unwrap(), *value);
            assert_eq!(machine.cpu.read_u32(output + 4).unwrap(), *error);
        }
        assert_eq!(machine.cpu.read_u32(callback_called).unwrap(), 0);
        assert_eq!(machine.cpu.read_u32(page).unwrap(), 56);
        assert_eq!(machine.cpu.read_u32(header).unwrap(), 52);
        assert_eq!(machine.sys.objects.object_count(), before + 1);
        assert!(machine.ext.get::<Win32>().user.top.is_empty());
    }
}
