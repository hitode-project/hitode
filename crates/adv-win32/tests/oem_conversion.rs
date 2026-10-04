//! OEM/ANSI conversion over authored paths and bytes; no host settings are changed.
use adv_abi::{ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const READ_ONLY: u32 = 0x420000;
const INPUTS: &[&[u8]] = &[
    b"C:\\Game\\fixture.exe\0",
    &[0x82, 0xa0, b'A', 0],
    &[0xd6, 0xd0, 0],
    &[0x81, 0],
    &[0xff, 0],
    b"\0",
    b"A\0unused",
];

fn registry() -> Registry {
    let mut reg = Registry::default();
    adv_win32::register(&mut reg);
    reg
}
fn machine<B: Backend>(cpu: B, codepage: u32) -> Machine<B> {
    let mut machine = Machine::new(cpu, registry(), Clock::virtual_time()).unwrap();
    let mut config = Win32Config::new("/nonexistent/oem", "fixture.exe", "/nonexistent/oem-data");
    config.os.codepage = codepage;
    Win32::install(&mut machine, config).unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(READ_ONLY, 4096, Prot::READ).unwrap();
    machine
}
fn conversions<B: Backend>(cpu: B, codepage: u32, in_place: bool) {
    let mut machine = machine(cpu, codepage);
    let convert = machine.resolve_import("user32.dll", "OemToCharA").unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    for (i, input) in INPUTS.iter().enumerate() {
        let source = DATA + i as u32 * 128;
        let output = if in_place { source } else { source + 32 };
        machine.cpu.write(source, &[0xa5; 64]).unwrap();
        machine.cpu.write(source, input).unwrap();
        code.push_imm(0x1234).call(set_error);
        code.push_imm(output)
            .push_imm(source)
            .call(convert)
            .mov_mem_eax(source + 64);
        code.call(get_error).mov_mem_eax(source + 68);
    }
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("OEM bytes", CODE, &[], 65536).unwrap();
    assert!(matches!(
        machine.run_virtual(1_000_000),
        RunResult::Exited(0)
    ));
    for (i, input) in INPUTS.iter().enumerate() {
        let source = DATA + i as u32 * 128;
        let mut expected = [0xa5; 32];
        let length = if in_place {
            input.len()
        } else {
            input.iter().position(|b| *b == 0).unwrap() + 1
        };
        expected[..length].copy_from_slice(&input[..length]);
        let mut output = [0; 32];
        machine
            .cpu
            .read(if in_place { source } else { source + 32 }, &mut output)
            .unwrap();
        assert_eq!(
            output, expected,
            "cp={codepage} inplace={in_place} input={input:?}"
        );
        assert_eq!(machine.cpu.read_u32(source + 64).unwrap(), 1);
        assert_eq!(machine.cpu.read_u32(source + 68).unwrap(), 0x1234);
    }
}

#[test]
fn oem_conversion_writes_actual_bytes_and_nul_in_place_or_separate() {
    for codepage in [932, 936, 1252] {
        for in_place in [false, true] {
            for jit in [false, true] {
                conversions(JitCpu::new(PARK_ADDR, jit).unwrap(), codepage, in_place);
            }
            conversions(UnicornCpu::new(PARK_ADDR).unwrap(), codepage, in_place);
        }
    }
    let reg = registry();
    assert!(reg.dynamic_available("user32.dll", "OemToCharA"));
    assert_eq!(
        reg.level("user32.dll", "OemToCharA"),
        Some(ImplementationLevel::Implement)
    );
    assert!(reg.signature_conflicts().is_empty());
}

fn protected<B: Backend>(cpu: B, output: u32) {
    let mut machine = machine(cpu, 932);
    let convert = machine.resolve_import("user32.dll", "OemToCharA").unwrap();
    machine.cpu.write(DATA, b"ABC\0").unwrap();
    machine.cpu.write(output, &[0x77; 2]).unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(output).push_imm(DATA).call(convert).ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("protected OEM output", CODE, &[], 65536)
        .unwrap();
    let RunResult::Failed(error) = machine.run_virtual(1_000_000) else {
        panic!("protected output accepted");
    };
    assert!(
        error.message.contains("ABI guest memory fault"),
        "{}",
        error.message
    );
    let mut bytes = [0; 2];
    machine.cpu.read(output, &mut bytes).unwrap();
    assert_eq!(bytes, [0x77; 2]);
}
#[test]
fn oem_conversion_validates_actual_output_before_overlapping_page_write() {
    for output in [READ_ONLY, DATA + 4094] {
        for jit in [false, true] {
            protected(JitCpu::new(PARK_ADDR, jit).unwrap(), output);
        }
        protected(UnicornCpu::new(PARK_ADDR).unwrap(), output);
    }
}

#[cfg(windows)]
#[test]
fn same_codepage_bytes_match_native_oem_conversion_without_locale_mutation() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetACP() -> u32;
        fn GetOEMCP() -> u32;
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn OemToCharA(source: *const u8, destination: *mut u8) -> i32;
    }
    if unsafe { GetACP() != GetOEMCP() } {
        return;
    } // The guest currently uses a shared OEM/ANSI page.
    for input in INPUTS {
        for in_place in [false, true] {
            let mut source = [0xa5; 32];
            source[..input.len()].copy_from_slice(input);
            let mut output = [0xa5; 32];
            let destination = if in_place {
                source.as_mut_ptr()
            } else {
                output.as_mut_ptr()
            };
            assert_ne!(unsafe { OemToCharA(source.as_ptr(), destination) }, 0);
            let mut expected = [0xa5; 32];
            let length = if in_place {
                input.len()
            } else {
                input.iter().position(|b| *b == 0).unwrap() + 1
            };
            expected[..length].copy_from_slice(&input[..length]);
            assert_eq!(if in_place { source } else { output }, expected);
        }
    }
}
