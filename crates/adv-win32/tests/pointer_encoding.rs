//! Authored pointer words validate process state, fixed forwarders and live RTL hooks.
use adv_abi::{Conv, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_os::{Loader, Modules};
use adv_pe::ImportName;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x420000;
#[derive(Clone, Copy)]
enum Arg {
    Value(u32),
    Stored(u32),
}
use Arg::{Stored as M, Value as I};
fn fixture(jit: bool) -> Machine<JitCpu> {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    for name in ["RtlEncodePointer", "RtlDecodePointer"] {
        assert_eq!(
            registry.get("ntdll.dll", name).unwrap().conv,
            Conv::Stdcall { argc: 1 }
        );
        assert!(registry.dynamic_available("ntdll.dll", name));
    }
    let mut machine = Machine::new(
        JitCpu::new(PARK_ADDR, jit).unwrap(),
        registry,
        Clock::virtual_time(),
    )
    .unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/pointer-encoding",
            "pointer.exe",
            "/nonexistent/pointer-encoding-data",
        ),
    )
    .unwrap();
    Win32::prepare_process_infrastructure(&mut machine).unwrap();
    machine.cpu.map(CODE, 0x10000, Prot::RX).unwrap();
    machine.cpu.map(DATA, 0x2000, Prot::RW).unwrap();
    machine
}
fn public(machine: &mut Machine<JitCpu>, module: &str, name: &str) -> u32 {
    let (win32, modules) = machine.ext.get2_mut::<Win32, Modules>();
    let id = modules.by_name(module).unwrap();
    Loader::new(&mut machine.cpu, &mut machine.sys, modules, &win32.vfs)
        .resolve_dynamic(
            id,
            &ImportName::Name {
                hint: 0,
                name: name.into(),
            },
        )
        .unwrap()
        .0
}
fn save_esp(code: &mut Asm, output: u32) {
    code.bytes.extend_from_slice(&[0x89, 0x25]);
    code.bytes.extend_from_slice(&output.to_le_bytes());
}
fn at(code: &mut Asm, entry: u32, args: &[Arg], output: u32, cdecl: bool) {
    save_esp(code, output + 4);
    for arg in args.iter().rev() {
        match arg {
            I(value) => {
                code.push_imm(*value);
            }
            M(pointer) => {
                code.mov_eax_mem(*pointer).push_eax();
            }
        }
    }
    code.call(entry).mov_mem_eax(output);
    if cdecl {
        code.add_esp(args.len() as u8 * 4);
    }
    save_esp(code, output + 8);
}
fn call(
    machine: &mut Machine<JitCpu>,
    code: &mut Asm,
    module: &str,
    name: &str,
    args: &[Arg],
    output: u32,
) {
    let entry = machine.resolve_import(module, name).unwrap();
    at(code, entry, args, output, module.starts_with("msvcr"));
}
fn finish(machine: &mut Machine<JitCpu>, mut code: Asm) {
    let exit = machine
        .resolve_import("kernel32.dll", "ExitProcess")
        .unwrap();
    code.push_imm(0).call(exit);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("pointer caller", CODE, &[], 0x10000).unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
}
fn value(machine: &Machine<JitCpu>, output: u32) -> u32 {
    assert_eq!(
        machine.cpu.read_u32(output + 4).unwrap(),
        machine.cpu.read_u32(output + 8).unwrap(),
        "both call conventions restore the caller stack"
    );
    machine.cpu.read_u32(output).unwrap()
}

#[test]
fn kernel_rtl_crt_and_a_second_thread_share_roundtrip_null_and_process_lifetime_state() {
    for jit in [false, true] {
        let mut machine = fixture(jit);
        let public_encode = public(&mut machine, "kernel32.dll", "EncodePointer");
        assert_eq!(
            public_encode,
            public(&mut machine, "ntdll.dll", "RtlEncodePointer")
        );
        let mut code = Asm::new(CODE);
        code.push_imm(0x12345678).call(
            machine
                .resolve_import("kernel32.dll", "SetLastError")
                .unwrap(),
        );
        for (i, pointer) in [0, 1, 0x400000, 0xdeadbeef, u32::MAX]
            .into_iter()
            .enumerate()
        {
            let output = DATA + i as u32 * 0x100;
            for (offset, module, name) in [
                (0, "kernel32.dll", "EncodePointer"),
                (0x10, "ntdll.dll", "RtlEncodePointer"),
                (0x20, "msvcr80.dll", "_encode_pointer"),
                (0x30, "msvcr90.dll", "_encode_pointer"),
            ] {
                call(
                    &mut machine,
                    &mut code,
                    module,
                    name,
                    &[I(pointer)],
                    output + offset,
                );
            }
            at(
                &mut code,
                public_encode,
                &[I(pointer)],
                output + 0x40,
                false,
            );
            for (offset, module, name, source) in [
                (0x50, "kernel32.dll", "DecodePointer", 0x20),
                (0x60, "ntdll.dll", "RtlDecodePointer", 0x40),
                (0x70, "msvcr80.dll", "_decode_pointer", 0x10),
                (0x80, "msvcr90.dll", "_decode_pointer", 0),
            ] {
                call(
                    &mut machine,
                    &mut code,
                    module,
                    name,
                    &[M(output + source)],
                    output + offset,
                );
            }
        }
        call(
            &mut machine,
            &mut code,
            "msvcr80.dll",
            "_encoded_null",
            &[],
            DATA + 0x900,
        );
        call(
            &mut machine,
            &mut code,
            "msvcr90.dll",
            "_encoded_null",
            &[],
            DATA + 0x920,
        );
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "GetLastError",
            &[],
            DATA + 0x940,
        );
        let worker = CODE + 0x3000;
        let mut routine = Asm::new(worker);
        call(
            &mut machine,
            &mut routine,
            "kernel32.dll",
            "EncodePointer",
            &[I(0xdeadbeef)],
            DATA + 0x960,
        );
        routine.bytes.extend_from_slice(&[0x31, 0xc0]);
        routine.ret_n(4);
        machine.cpu.write(worker, &routine.bytes).unwrap();
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "CreateThread",
            &[I(0), I(0), I(worker), I(0), I(0), I(0)],
            DATA + 0x980,
        );
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "WaitForSingleObject",
            &[M(DATA + 0x980), I(u32::MAX)],
            DATA + 0x9a0,
        );
        finish(&mut machine, code);
        for (i, pointer) in [0, 1, 0x400000, 0xdeadbeef, u32::MAX]
            .into_iter()
            .enumerate()
        {
            let output = DATA + i as u32 * 0x100;
            let encoded = value(&machine, output);
            for offset in [0x10, 0x20, 0x30, 0x40] {
                assert_eq!(value(&machine, output + offset), encoded);
            }
            for offset in [0x50, 0x60, 0x70, 0x80] {
                assert_eq!(value(&machine, output + offset), pointer);
            }
        }
        assert_ne!(
            value(&machine, DATA),
            0,
            "encoded NULL is a process cookie value"
        );
        assert_eq!(value(&machine, DATA + 0x900), value(&machine, DATA));
        assert_eq!(value(&machine, DATA + 0x920), value(&machine, DATA));
        assert_eq!(value(&machine, DATA + 0x940), 0x12345678);
        assert_eq!(value(&machine, DATA + 0x960), value(&machine, DATA + 0x300));
        assert_eq!(value(&machine, DATA + 0x9a0), 0);
    }
}

fn hook(machine: &mut Machine<JitCpu>, name: &str, address: u32, observed: u32, mask: u32) {
    let entry = public(machine, "ntdll.dll", name);
    let mut code = Asm::new(address);
    code.mov_eax_mem(observed)
        .add_eax_imm(1)
        .mov_mem_eax(observed)
        .mov_eax_esp(4)
        .mov_mem_eax(observed + 4);
    code.bytes.push(0x35);
    code.bytes.extend_from_slice(&mask.to_le_bytes());
    code.ret_n(4);
    machine.cpu.write(address, &code.bytes).unwrap();
    let mut patch = Asm::new(entry);
    patch.jmp(address);
    machine.cpu.write(entry, &patch.bytes).unwrap();
}
#[test]
fn host_fronts_fixed_forwarders_and_crt_helpers_execute_live_rtl_hooks_and_pointer_returns() {
    const MASK: u32 = 0xa55aa55a;
    const POINTER: u32 = 0x12345678;
    for jit in [false, true] {
        let mut machine = fixture(jit);
        let public_kernel = public(&mut machine, "kernel32.dll", "EncodePointer");
        let public_rtl = public(&mut machine, "ntdll.dll", "RtlEncodePointer");
        hook(
            &mut machine,
            "RtlEncodePointer",
            CODE + 0x4000,
            DATA + 0x800,
            MASK,
        );
        hook(
            &mut machine,
            "RtlDecodePointer",
            CODE + 0x5000,
            DATA + 0x820,
            MASK,
        );
        let mut code = Asm::new(CODE);
        code.push_imm(0x12345678).call(
            machine
                .resolve_import("kernel32.dll", "SetLastError")
                .unwrap(),
        );
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "EncodePointer",
            &[I(POINTER)],
            DATA,
        );
        at(&mut code, public_rtl, &[I(POINTER)], DATA + 0x20, false);
        at(&mut code, public_kernel, &[I(POINTER)], DATA + 0x40, false);
        call(
            &mut machine,
            &mut code,
            "msvcr80.dll",
            "_encode_pointer",
            &[I(POINTER)],
            DATA + 0x60,
        );
        call(
            &mut machine,
            &mut code,
            "msvcr90.dll",
            "_encode_pointer",
            &[I(POINTER)],
            DATA + 0x80,
        );
        call(
            &mut machine,
            &mut code,
            "msvcr80.dll",
            "_encoded_null",
            &[],
            DATA + 0xa0,
        );
        call(
            &mut machine,
            &mut code,
            "msvcr90.dll",
            "_encoded_null",
            &[],
            DATA + 0xc0,
        );
        for (offset, module, name, source) in [
            (0x100, "kernel32.dll", "DecodePointer", 0),
            (0x120, "ntdll.dll", "RtlDecodePointer", 0x20),
            (0x140, "msvcr80.dll", "_decode_pointer", 0x60),
            (0x160, "msvcr90.dll", "_decode_pointer", 0x80),
            (0x180, "kernel32.dll", "DecodePointer", 0xa0),
        ] {
            if module == "ntdll.dll" {
                let entry = public(&mut machine, module, name);
                at(&mut code, entry, &[M(DATA + source)], DATA + offset, false);
            } else {
                call(
                    &mut machine,
                    &mut code,
                    module,
                    name,
                    &[M(DATA + source)],
                    DATA + offset,
                );
            }
        }
        call(
            &mut machine,
            &mut code,
            "kernel32.dll",
            "GetLastError",
            &[],
            DATA + 0x1a0,
        );
        finish(&mut machine, code);
        for offset in [0, 0x20, 0x40, 0x60, 0x80] {
            assert_eq!(value(&machine, DATA + offset), POINTER ^ MASK);
        }
        assert_eq!(
            (value(&machine, DATA + 0xa0), value(&machine, DATA + 0xc0)),
            (MASK, MASK)
        );
        for offset in [0x100, 0x120, 0x140, 0x160] {
            assert_eq!(value(&machine, DATA + offset), POINTER);
        }
        assert_eq!(value(&machine, DATA + 0x180), 0);
        assert_eq!(value(&machine, DATA + 0x1a0), 0x12345678);
        assert_eq!(
            (
                machine.cpu.read_u32(DATA + 0x800).unwrap(),
                machine.cpu.read_u32(DATA + 0x820).unwrap()
            ),
            (7, 5)
        );
        assert_eq!(machine.cpu.read_u32(DATA + 0x804).unwrap(), 0);
    }
}
