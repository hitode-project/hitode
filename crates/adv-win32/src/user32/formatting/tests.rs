//! Authored x86 stack/va_list inputs; oracle is in target/.../wsprintf-native.
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const OUTPUT: u32 = DATA + 0x2000;
const FORMAT: u32 = DATA + 0x100;
const ARGUMENTS: u32 = DATA + 0x1000;

fn bare<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    for name in ["wsprintfA", "wsprintfW"] {
        assert_eq!(
            registry.get(USER32, name).unwrap().conv,
            Conv::CdeclVariadic { fixed: 2 }
        );
    }
    for name in ["wvsprintfA", "wvsprintfW"] {
        assert_eq!(
            registry.get(USER32, name).unwrap().conv,
            Conv::Stdcall { argc: 3 }
        );
    }
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/wsprintf-tests");
    Win32::install(
        &mut machine,
        crate::Win32Config::new(&directory, "authored.exe", &directory),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 0x4000, Prot::RW).unwrap();
    machine
}
fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut machine = bare(cpu);
    machine
        .spawn("printf owner", PARK_ADDR, &[], 65536)
        .unwrap();
    machine.cpu.set_reg(Reg::Esp, DATA);
    machine
}
fn wide<B: Backend>(machine: &mut Machine<B>, pointer: u32, text: &[u16]) {
    let bytes: Vec<_> = text.iter().copied().flat_map(u16::to_le_bytes).collect();
    machine.cpu.write(pointer, &bytes).unwrap();
}
fn invoke<B: Backend>(
    machine: &mut Machine<B>,
    name: &str,
    format: &str,
    values: &[u32],
    unicode: bool,
) -> u32 {
    if unicode {
        wide(
            machine,
            FORMAT,
            &format.encode_utf16().chain([0]).collect::<Vec<_>>(),
        );
    } else {
        machine
            .cpu
            .write(FORMAT, &[format.as_bytes(), &[0]].concat())
            .unwrap();
    }
    for (index, value) in values.iter().enumerate() {
        machine
            .cpu
            .write_u32(ARGUMENTS + index as u32 * 4, *value)
            .unwrap();
    }
    machine.cpu.write_u32(DATA, 0x12345678).unwrap();
    for (index, value) in [OUTPUT, FORMAT, ARGUMENTS].iter().enumerate() {
        machine
            .cpu
            .write_u32(DATA + 4 + index as u32 * 4, *value)
            .unwrap();
    }
    let handler = (machine.sys.thunks.resolver() as &dyn std::any::Any)
        .downcast_ref::<Registry>()
        .unwrap()
        .get(USER32, name)
        .unwrap()
        .handler;
    match handler(&mut Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }) {
        Outcome::Return(result) => result,
        _ => panic!("formatting must return synchronously"),
    }
}
fn result<B: Backend>(machine: &Machine<B>, count: usize, unicode: bool) -> String {
    let mut bytes = vec![0; count * if unicode { 2 } else { 1 }];
    machine.cpu.read(OUTPUT, &mut bytes).unwrap();
    if unicode {
        String::from_utf16(
            &bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|bytes| u16::from_le_bytes(*bytes))
                .collect::<Vec<_>>(),
        )
        .unwrap()
    } else {
        machine.ext.get::<Win32>().codec.decode(&bytes)
    }
}

#[test]
fn windows_mixed_strings_characters_integer_modifiers_and_guest_codepage_match_native_profile() {
    let mut machine = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    let japanese = DATA + 0x500;
    let ansi = DATA + 0x600;
    wide(
        &mut machine,
        japanese,
        &"漢字\0".encode_utf16().collect::<Vec<_>>(),
    );
    machine.ext.get_mut::<Win32>().codec = adv_os::Codec::new(936);
    machine
        .cpu
        .write(ansi, &[0xd6, 0xd0, 0xce, 0xc4, 0])
        .unwrap(); // GBK 中文.
    let count = invoke(
        &mut machine,
        "wvsprintfW",
        "%s|%S|%hs|%hS|%ls|%lS",
        &[japanese, ansi, ansi, ansi, japanese, japanese],
        true,
    );
    assert_eq!(
        result(&machine, count as usize, true),
        "漢字|中文|中文|中文|漢字|漢字"
    );
    let count = invoke(
        &mut machine,
        "wvsprintfA",
        "%s|%S|%hS|%ls",
        &[ansi, japanese, ansi, japanese],
        false,
    );
    assert_eq!(
        result(&machine, count as usize, false),
        "中文|漢字|中文|漢字"
    );
    let count = invoke(
        &mut machine,
        "wvsprintfW",
        "%c|%C|%hc|%hC|%lc|%lC",
        &[0x3042, 65, 66, 67, 0x6f22, 0x5b57],
        true,
    );
    assert_eq!(count, 11);
    assert_eq!(result(&machine, count as usize, true), "あ|A|B|C|漢|字");
    let count = invoke(
        &mut machine,
        "wvsprintfW",
        "%hd|%hu|%hx|%d|%I64x",
        &[
            0x12348001,
            0x1234abcd,
            0x12345678,
            u32::MAX,
            0x89abcdef,
            0x12345678,
        ],
        true,
    );
    assert_eq!(
        result(&machine, count as usize, true),
        "-32767|305441741|12345678|-1|1234567889abcdef"
    );
    let count = invoke(
        &mut machine,
        "wvsprintfW",
        "%o|%f|%q|%*s|%+d|% d|%u",
        &[17, 18, 19, 20],
        true,
    );
    assert_eq!(result(&machine, count as usize, true), "o|f|q|*s|+d| d|17");
    let count = invoke(&mut machine, "wvsprintfW", "%s|%S", &[0, 0], true);
    assert_eq!(count, 1);
    assert_eq!(result(&machine, 1, true), "|");
    assert_eq!(
        invoke(
            &mut machine,
            "wvsprintfW",
            "%cEND|%lcEND|%hcEND",
            &[0, 0, 0],
            true
        ),
        14
    );
    assert_eq!(result(&machine, 14, true), "\0END|\0END|\0END");
}

#[test]
fn bounded_output_uses_1024_elements_plus_terminator_and_invalid_memory_is_untouched() {
    let mut machine = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    let source = DATA + 0x500;
    for unicode in [false, true] {
        for length in [511, 512, 1023, 1024, 1025, 1100] {
            machine.cpu.write(OUTPUT, &vec![0x77; 4096]).unwrap();
            if unicode {
                wide(
                    &mut machine,
                    source,
                    &vec![0x3042; length]
                        .into_iter()
                        .chain([0])
                        .collect::<Vec<_>>(),
                );
            } else {
                machine
                    .cpu
                    .write(source, &[vec![b'a'; length], vec![0]].concat())
                    .unwrap();
            }
            let api = if unicode { "wvsprintfW" } else { "wvsprintfA" };
            let count = invoke(&mut machine, api, "%s", &[source], unicode);
            assert_eq!(count as usize, length.min(1024));
            let stride = if unicode { 2 } else { 1 };
            let mut edge = [0; 4];
            machine
                .cpu
                .read(OUTPUT + count * stride, &mut edge[..stride as usize * 2])
                .unwrap();
            assert_eq!(&edge[..stride as usize], &vec![0; stride as usize]);
            assert_eq!(
                &edge[stride as usize..stride as usize * 2],
                &vec![0x77; stride as usize]
            );
        }
    }
    for (format, first) in [("%1024d", ' '), ("%1025d", '7'), ("%.1025d", '7')] {
        assert_eq!(invoke(&mut machine, "wvsprintfW", format, &[7], true), 1024);
        assert_eq!(result(&machine, 1, true), first.to_string());
    }
    machine.cpu.write(OUTPUT, &[0x77; 32]).unwrap();
    assert_eq!(
        invoke(&mut machine, "wvsprintfW", "%s", &[0xdead0000], true),
        0
    );
    let mut unchanged = [0; 32];
    machine.cpu.read(OUTPUT, &mut unchanged).unwrap();
    assert_eq!(unchanged, [0x77; 32]);
    let crossing = OUTPUT + 4094;
    machine.cpu.write(crossing, &[0x55; 8]).unwrap();
    machine
        .cpu
        .protect(OUTPUT + 4096, 4096, Prot::READ)
        .unwrap();
    invoke(&mut machine, "wvsprintfW", "AB", &[], true);
    machine.cpu.write_u32(DATA + 4, crossing).unwrap();
    let handler = wvsprintf_w_ABI.handler;
    let mut env = Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    };
    assert!(matches!(handler(&mut env), Outcome::Return(0)));
    assert_eq!(crate::util::last_error(&env), 87);
    env.cpu.read(crossing, &mut unchanged[..8]).unwrap();
    assert_eq!(&unchanged[..8], &[0x55; 8]);
}

fn cdecl<B: Backend>(cpu: B) {
    let mut machine = bare(cpu);
    wide(
        &mut machine,
        FORMAT,
        &"CPU %s: %u MHz %#x\0".encode_utf16().collect::<Vec<_>>(),
    );
    let source = DATA + 0x500;
    wide(
        &mut machine,
        source,
        &"機種\0".encode_utf16().collect::<Vec<_>>(),
    );
    let printf = machine.resolve_import(USER32, "wsprintfW").unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let exit = machine
        .resolve_import("kernel32.dll", "ExitProcess")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(0x76543210).call(set_error);
    code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax,esp
    code.mov_mem_eax(DATA + 0x50);
    for arg in [0x24, 3200, source, FORMAT, OUTPUT] {
        code.push_imm(arg);
    }
    code.call(printf).mov_mem_eax(DATA + 0x54).add_esp(20);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 0x58);
    code.call(get_error).mov_mem_eax(DATA + 0x5c);
    code.push_imm(0).call(exit);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine.spawn("WINAPIV", CODE, &[], 65536).unwrap();
    let run = machine.run_virtual(1_000_000);
    assert!(matches!(run, RunResult::Exited(0)), "{run:?}");
    let count = machine.cpu.read_u32(DATA + 0x54).unwrap();
    assert_eq!(
        result(&machine, count as usize, true),
        "CPU 機種: 3200 MHz 0x24"
    );
    assert_eq!(
        machine.cpu.read_u32(DATA + 0x50).unwrap(),
        machine.cpu.read_u32(DATA + 0x58).unwrap()
    );
    assert_eq!(machine.cpu.read_u32(DATA + 0x5c).unwrap(), 0x76543210);
}
#[test]
fn actual_guest_cdecl_stack_is_caller_cleaned_and_last_error_is_preserved() {
    cdecl(UnicornCpu::new(PARK_ADDR).unwrap());
    cdecl(JitCpu::new(PARK_ADDR, false).unwrap());
}
