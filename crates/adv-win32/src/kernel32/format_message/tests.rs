//! Authored native kernel32 oracle: target/investigations/katahane-format-message/native.
//! Probe source SHA256: 63D4B2D0FF28993CE90C8567CC7415A7301E2E9CC2ECBE6443B95FBFCA66561B.
use super::*;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, Reg, asm::Asm},
};
use adv_machine::{Clock, Conv, Machine, Outcome, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const OUT: u32 = 0x420000;
const SENTINEL: u32 = 0x12345678;

fn fixture<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    for name in ["FormatMessageA", "FormatMessageW"] {
        assert_eq!(
            registry.get("kernel32.dll", name).unwrap().conv,
            Conv::Stdcall { argc: 7 }
        );
    }
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/scratch/katahane_format_message/tests");
    std::fs::create_dir_all(&root).unwrap();
    Win32::install(
        &mut machine,
        crate::Win32Config::new(root.clone(), "authored.exe", root.join("data")),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(OUT, 0x50000, Prot::RW).unwrap();
    machine.spawn("FormatMessage", CODE, &[], 65536).unwrap();
    machine
}
fn env<B: Backend>(machine: &mut Machine<B>) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: 0,
    }
}
fn bytes<B: Backend>(machine: &Machine<B>, pointer: u32, count: usize) -> Vec<u8> {
    let mut output = vec![0; count];
    machine.cpu.read(pointer, &mut output).unwrap();
    output
}
fn wide<B: Backend>(machine: &mut Machine<B>, pointer: u32, text: &str) {
    let bytes: Vec<_> = text
        .encode_utf16()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    machine.cpu.write(pointer, &bytes).unwrap();
}
fn narrow<B: Backend>(machine: &mut Machine<B>, pointer: u32, text: &[u8]) {
    machine.cpu.write(pointer, &[text, &[0]].concat()).unwrap();
}
fn invoke<B: Backend>(machine: &mut Machine<B>, values: [u32; 7], wide: bool) -> u32 {
    // Exercise generated argument extraction with a real x86 parameter stack.
    machine.cpu.set_reg(Reg::Esp, DATA);
    for (index, value) in values.iter().enumerate() {
        machine
            .cpu
            .write_u32(DATA + 4 + index as u32 * 4, *value)
            .unwrap();
    }
    let handler = if wide {
        format_w_ABI.handler
    } else {
        format_a_ABI.handler
    };
    match handler(&mut env(machine)) {
        Outcome::Return(value) => value,
        _ => panic!("formatting must return synchronously"),
    }
}
fn error<B: Backend>(machine: &mut Machine<B>) -> u32 {
    crate::util::last_error(&env(machine))
}
fn failure_code(result: Result<Vec<u16>, Failure>) -> u32 {
    match result {
        Err(Failure::Error(error) | Failure::Unavailable(_, error)) => error,
        Ok(_) => panic!("expected failure"),
    }
}
fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

#[test]
fn native_control_escapes_and_ignore_inserts() {
    for (source, ignore, single, expected) in [
        ("A%%B%tC%rD%nE%.%!% Q", false, false, "A%B\tC\rD\r\nE.! Q"),
        (
            "A%%B%tC%rD%nE%.%!% Q",
            true,
            false,
            "A%%B\tC\rD\r\nE%.%!% Q",
        ),
        ("A%0unused", true, false, "A"),
        ("A%1!s!B", true, false, "A%1B"),
        ("A%1!03d!B", true, false, "A%1!03d!B"),
        ("A\r\nB\nC\rD", true, false, "A\r\nB\r\nC\r\nD"),
        ("A\r\nB\nC\rD", true, true, "A B C D"),
        ("A%n\r\nB", true, true, "A\r\n B"),
        ("A\n\nB", true, true, "A  B"),
        ("A%zB", false, false, "AzB"),
        ("A%zB", true, false, "A%zB"),
    ] {
        let result = render(
            |_, _| panic!("unused insertion"),
            &units(source),
            ignore,
            single,
        );
        assert!(
            matches!(&result, Ok(value) if *value == units(expected)),
            "{source}"
        );
    }
    for source in ["", "%0"] {
        assert_eq!(
            failure_code(render(|_, _| unreachable!(), &units(source), true, false)),
            235
        );
    }
    for source in ["tail%", "A%1!missing"] {
        assert_eq!(
            failure_code(render(|_, _| unreachable!(), &units(source), true, false)),
            87
        );
    }
}

fn actual_guest<B: Backend>(cpu: B) {
    let mut machine = fixture(cpu);
    wide(&mut machine, DATA + 0x100, "%1/%2!u!/%1!s!");
    wide(&mut machine, DATA + 0x180, "東京");
    machine.cpu.write_u32(DATA + 0x200, DATA + 0x180).unwrap();
    machine.cpu.write_u32(DATA + 0x204, 42).unwrap();
    let format_a = machine
        .resolve_import("kernel32.dll", "FormatMessageA")
        .unwrap();
    let format_w = machine
        .resolve_import("kernel32.dll", "FormatMessageW")
        .unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let free = machine.resolve_import("kernel32.dll", "LocalFree").unwrap();
    let local_size = machine.resolve_import("kernel32.dll", "LocalSize").unwrap();
    let exit = machine
        .resolve_import("kernel32.dll", "ExitProcess")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.bytes.extend_from_slice(&[0x89, 0xe0]); // mov eax,esp
    code.mov_mem_eax(DATA + 0x300);
    code.push_imm(SENTINEL).call(set_error);
    for value in [
        ALLOCATE_BUFFER | FROM_SYSTEM | IGNORE_INSERTS,
        0,
        2,
        0x409,
        DATA + 0x380,
        128,
        0,
    ]
    .into_iter()
    .rev()
    {
        code.push_imm(value);
    }
    code.call(format_a).mov_mem_eax(DATA + 0x304);
    code.call(get_error).mov_mem_eax(DATA + 0x308);
    for value in [
        FROM_STRING | ARGUMENT_ARRAY,
        DATA + 0x100,
        0,
        0,
        OUT,
        64,
        DATA + 0x200,
    ]
    .into_iter()
    .rev()
    {
        code.push_imm(value);
    }
    code.call(format_w).mov_mem_eax(DATA + 0x30c);
    code.call(get_error).mov_mem_eax(DATA + 0x310);
    code.bytes.extend_from_slice(&[0xff, 0x35]); // push dword ptr [allocated pointer]
    code.bytes.extend_from_slice(&(DATA + 0x380).to_le_bytes());
    code.call(local_size).mov_mem_eax(DATA + 0x314);
    code.bytes.extend_from_slice(&[0xff, 0x35]);
    code.bytes.extend_from_slice(&(DATA + 0x380).to_le_bytes());
    code.call(free).mov_mem_eax(DATA + 0x318);
    code.bytes.extend_from_slice(&[0x89, 0xe0]);
    code.mov_mem_eax(DATA + 0x31c);
    code.push_imm(0).call(exit);
    machine.cpu.write(CODE, &code.bytes).unwrap();
    let before = machine.sys.objects.object_count();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_eq!(
        machine.cpu.read_u32(DATA + 0x300).unwrap(),
        machine.cpu.read_u32(DATA + 0x31c).unwrap()
    );
    assert_eq!(machine.cpu.read_u32(DATA + 0x304).unwrap(), 44);
    assert_eq!(machine.cpu.read_u32(DATA + 0x308).unwrap(), SENTINEL);
    assert_eq!(machine.cpu.read_u32(DATA + 0x30c).unwrap(), 8);
    assert_eq!(machine.cpu.read_u32(DATA + 0x310).unwrap(), SENTINEL);
    assert!(machine.cpu.read_u32(DATA + 0x314).unwrap() >= 128);
    assert_eq!(machine.cpu.read_u32(DATA + 0x318).unwrap(), 0);
    assert_eq!(
        bytes(&machine, OUT, 18),
        "東京/42/東京\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    );
    assert_eq!(machine.sys.objects.object_count(), before);
    let pointer = machine.cpu.read_u32(DATA + 0x380).unwrap();
    assert_eq!(
        machine.ext.get::<Heaps>().owner(&machine.cpu, pointer),
        None
    );
    assert_eq!(
        machine
            .ext
            .get::<Globals>()
            .from_pointer(&machine.sys.objects, pointer),
        None
    );
}
#[test]
fn jit_stdcall_and_local_free() {
    actual_guest(JitCpu::new(PARK_ADDR, false).unwrap());
}
#[test]
fn unicorn_stdcall_and_local_free() {
    actual_guest(UnicornCpu::new(PARK_ADDR).unwrap());
}

#[test]
fn ansi_codepages_and_x86_va_list_pointer() {
    let mut machine = fixture(JitCpu::new(PARK_ADDR, false).unwrap());
    for (cp, text) in [(932, "東京"), (936, "中文"), (1252, "café"), (65001, "😀")] {
        machine.ext.get_mut::<Win32>().codec.acp = cp;
        let encoded = machine.ext.get::<Win32>().codec.encode(text);
        narrow(&mut machine, DATA + 0x100, b"%1/%2!d!");
        narrow(&mut machine, DATA + 0x180, &encoded);
        machine.cpu.write_u32(DATA + 0x200, DATA + 0x180).unwrap();
        machine
            .cpu
            .write_u32(DATA + 0x204, (-42i32) as u32)
            .unwrap();
        machine.cpu.write_u32(DATA + 0x220, DATA + 0x200).unwrap();
        set_last_error(&mut env(&mut machine), SENTINEL);
        let result = invoke(
            &mut machine,
            [FROM_STRING, DATA + 0x100, 0, 0, OUT, 256, DATA + 0x220],
            false,
        );
        let expected = [encoded, b"/-42\0".to_vec()].concat();
        assert_eq!(result as usize, expected.len() - 1);
        assert_eq!(bytes(&machine, OUT, expected.len()), expected);
        assert_eq!(error(&mut machine), SENTINEL);
    }
}

#[test]
fn system_text_and_wide_allocations_use_local_memory_metadata() {
    let mut machine = fixture(JitCpu::new(PARK_ADDR, false).unwrap());
    set_last_error(&mut env(&mut machine), SENTINEL);
    assert_eq!(
        invoke(
            &mut machine,
            [FROM_SYSTEM | IGNORE_INSERTS, 0, 5, 0, OUT, 256, 0],
            false
        ),
        19
    );
    assert_eq!(bytes(&machine, OUT, 20), b"Access is denied.\r\n\0");
    assert_eq!(error(&mut machine), SENTINEL);
    wide(&mut machine, DATA + 0x100, "東京");
    assert_eq!(
        invoke(
            &mut machine,
            [FROM_STRING | ALLOCATE_BUFFER, DATA + 0x100, 0, 0, OUT, 8, 0],
            true
        ),
        2
    );
    let pointer = machine.cpu.read_u32(OUT).unwrap();
    assert_eq!(
        bytes(&machine, pointer, 6),
        "東京\0"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>()
    );
    let block = machine
        .ext
        .get::<Globals>()
        .get(&machine.sys.objects, pointer)
        .unwrap();
    assert!(!block.movable);
    assert!(
        block.size >= 16,
        "allocation minimum is measured in UTF16 units"
    );
    assert_eq!(error(&mut machine), SENTINEL);
    let (heaps, globals) = machine.ext.get2_mut::<Heaps, Globals>();
    globals
        .free(
            &mut machine.sys.objects,
            heaps,
            &mut machine.cpu,
            &mut machine.sys.vm,
            pointer,
            false,
        )
        .unwrap();
    assert_eq!(
        machine.ext.get::<Heaps>().owner(&machine.cpu, pointer),
        None
    );
}

#[test]
fn failures_leave_outputs_and_allocations_untouched() {
    let mut machine = fixture(JitCpu::new(PARK_ADDR, false).unwrap());
    narrow(&mut machine, DATA + 0x100, b"plain");
    narrow(&mut machine, DATA + 0x180, b"%1!08x!");
    machine.cpu.write_u32(DATA + 0x200, 7).unwrap();
    let cases = [
        ([FROM_STRING, DATA + 0x100, 0, 0, OUT, 5, 0], 122),
        ([FROM_STRING, DATA + 0x100, 0, 0, OUT, 0, 0], 122),
        ([FROM_STRING, 0, 0, 0, OUT, 256, 0], 87),
        (
            [FROM_STRING | FROM_SYSTEM, DATA + 0x100, 0, 0, OUT, 256, 0],
            87,
        ),
        ([FROM_SYSTEM, 0, u32::MAX, 0, OUT, 256, 0], 317),
        ([FROM_SYSTEM, 0, 2, 0x411, OUT, 256, 0], 1815),
        ([FROM_HMODULE | FROM_SYSTEM, 0, 2, 0, OUT, 256, 0], 50),
        ([FROM_STRING | 40, DATA + 0x100, 0, 0, OUT, 256, 0], 50),
        (
            [
                FROM_STRING | ARGUMENT_ARRAY,
                DATA + 0x180,
                0,
                0,
                OUT,
                256,
                DATA + 0x200,
            ],
            50,
        ),
        ([FROM_STRING, DATA + 0x180, 0, 0, OUT, 256, 0], 87),
        ([FROM_SYSTEM | ALLOCATE_BUFFER, 0, 2, 0, 0, 0, 0], 998),
        (
            [FROM_SYSTEM | ALLOCATE_BUFFER, 0, 2, 0, OUT, u32::MAX, 0],
            8,
        ),
        ([FROM_SYSTEM, 0, 2, 0, CODE, 256, 0], 998),
        ([FROM_STRING, CODE + 0x1000, 0, 0, OUT, 256, 0], 998),
    ];
    for (values, expected) in cases {
        machine.cpu.write(OUT, &[0xcc; 256]).unwrap();
        let before = machine.sys.objects.object_count();
        assert_eq!(invoke(&mut machine, values, false), 0);
        assert_eq!(error(&mut machine), expected);
        assert_eq!(bytes(&machine, OUT, 256), vec![0xcc; 256]);
        assert_eq!(machine.sys.objects.object_count(), before);
    }
    for source in [b"".as_slice(), b"%0", b"tail%", b"%1!missing"] {
        narrow(&mut machine, DATA + 0x100, source);
        machine.cpu.write(OUT, &[0xcc; 256]).unwrap();
        assert_eq!(
            invoke(
                &mut machine,
                [
                    FROM_STRING | IGNORE_INSERTS,
                    DATA + 0x100,
                    0,
                    0,
                    OUT,
                    256,
                    0
                ],
                false
            ),
            0
        );
        assert_eq!(
            error(&mut machine),
            if matches!(source, b"" | b"%0") {
                235
            } else {
                87
            }
        );
        assert_eq!(bytes(&machine, OUT, 256), vec![0xcc; 256]);
    }
}

#[test]
fn wide_counts_utf16_units_and_respects_message_size_limit() {
    let mut machine = fixture(JitCpu::new(PARK_ADDR, false).unwrap());
    wide(&mut machine, DATA + 0x100, "😀");
    assert_eq!(
        invoke(
            &mut machine,
            [FROM_STRING, DATA + 0x100, 0, 0, OUT, 3, 0],
            true
        ),
        2
    );
    let input = OUT + 0x1000;
    machine
        .cpu
        .write(input, &vec![b'a'; MAX_BYTES + 1])
        .unwrap();
    assert_eq!(
        invoke(&mut machine, [FROM_STRING, input, 0, 0, OUT, 256, 0], false),
        0
    );
    assert_eq!(error(&mut machine), 234);
    let repeated: Vec<_> = vec![u16::from(b'a'); MAX_BYTES / 2 + 1]
        .into_iter()
        .chain([0])
        .flat_map(u16::to_le_bytes)
        .collect();
    machine.cpu.write(input, &repeated).unwrap();
    assert_eq!(
        invoke(
            &mut machine,
            [FROM_STRING | ALLOCATE_BUFFER, input, 0, 0, OUT, 0, 0],
            true
        ),
        0
    );
    assert_eq!(error(&mut machine), 234);
}
