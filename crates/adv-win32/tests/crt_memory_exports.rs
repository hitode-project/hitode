//! Four actual VCRuntime imports share guest memory with the legacy CRT fronts.
use adv_abi::{Conv, ImplementationLevel, Registry};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use adv_win32::{Win32, Win32Config};

const MODULE: &str = "vcruntime140.dll";
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const SOURCE: u32 = 0x420000;
const SNAPSHOT: u32 = 0x430000;
const NOACCESS: u32 = 0x460000;
const SIZE: u32 = 0x10000;

#[derive(Clone, Copy)]
enum Arg {
    Value(u32),
    Saved(u32),
}
use Arg::{Saved as S, Value as V};

fn save_esp(code: &mut Asm, output: u32) {
    code.bytes.extend_from_slice(&[0x89, 0x25]);
    code.bytes.extend_from_slice(&output.to_le_bytes());
}

fn call<B: Backend>(
    machine: &mut Machine<B>,
    code: &mut Asm,
    module: &str,
    name: &str,
    args: &[Arg],
    output: u32,
    cdecl: bool,
) {
    save_esp(code, output + 4);
    for &arg in args.iter().rev() {
        match arg {
            V(value) => {
                code.push_imm(value);
            }
            S(pointer) => {
                code.mov_eax_mem(pointer).push_eax();
            }
        }
    }
    let entry = machine.resolve_import(module, name).unwrap();
    code.call(entry).mov_mem_eax(output);
    save_esp(code, output + 8);
    if cdecl {
        code.add_esp(args.len() as u8 * 4);
    }
    save_esp(code, output + 12);
}

fn memory_exports<B: Backend>(cpu: B) {
    let mut registry = Registry::default();
    adv_win32::register(&mut registry);
    adv_win32::msvcrt::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    for name in ["memchr", "memcpy", "memmove", "memset"] {
        assert_eq!(
            registry.get(MODULE, name).unwrap().conv,
            Conv::Cdecl { argc: 3 }
        );
        assert_eq!(
            registry.level(MODULE, name),
            Some(ImplementationLevel::Implement)
        );
        assert!(registry.dynamic_available(MODULE, name));
        let signature = adv_abi::signature::lookup(MODULE, name).unwrap();
        assert_eq!(signature.parameters.len(), 3);
        assert_eq!(signature.stack_words(), 3);
        assert_eq!(signature.return_kind, adv_abi::AbiKind::Pointer);
    }
    assert!(!registry.dynamic_available(MODULE, "memcmp"));
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    Win32::install(
        &mut machine,
        Win32Config::new(
            "/nonexistent/crt-memory",
            "fixture.exe",
            "/nonexistent/crt-memory-data",
        ),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine.cpu.map(SOURCE, SIZE, Prot::READ).unwrap();
    machine.cpu.map(SNAPSHOT, SIZE + 4096, Prot::RW).unwrap();
    machine.cpu.map(NOACCESS, 4096, Prot::default()).unwrap();
    let mut source: Vec<u8> = (0..SIZE).map(|index| (index % 251) as u8).collect();
    source[SIZE as usize - 1] = 0xff;
    machine.cpu.write(SOURCE, &source).unwrap();
    machine
        .cpu
        .write(SNAPSHOT, &vec![0xcc; SIZE as usize + 4096])
        .unwrap();
    machine.cpu.write(NOACCESS, &[0x77; 32]).unwrap();
    machine
        .cpu
        .write(DATA + 0xd00, &(0..32).collect::<Vec<u8>>())
        .unwrap();

    let mut code = Asm::new(CODE);
    code.push_imm(0x31415926).call(
        machine
            .resolve_import("kernel32.dll", "SetLastError")
            .unwrap(),
    );
    call(
        &mut machine,
        &mut code,
        "kernel32.dll",
        "VirtualAlloc",
        &[V(0), V(SIZE), V(0x3000), V(4)],
        DATA,
        false,
    );
    call(
        &mut machine,
        &mut code,
        MODULE,
        "memset",
        &[S(DATA), V(0x12345a), V(SIZE)],
        DATA + 0x20,
        true,
    );
    call(
        &mut machine,
        &mut code,
        MODULE,
        "memcpy",
        &[V(SNAPSHOT + 4), S(DATA), V(SIZE)],
        DATA + 0x40,
        true,
    );
    call(
        &mut machine,
        &mut code,
        MODULE,
        "memcpy",
        &[S(DATA), V(SOURCE), V(SIZE)],
        DATA + 0x60,
        true,
    );
    // Overlap in both directions belongs to memmove, never to the memcpy contract.
    code.mov_eax_mem(DATA)
        .add_eax_imm(5)
        .mov_mem_eax(DATA + 0x300);
    call(
        &mut machine,
        &mut code,
        MODULE,
        "memmove",
        &[S(DATA + 0x300), S(DATA), V(37)],
        DATA + 0x80,
        true,
    );
    call(
        &mut machine,
        &mut code,
        MODULE,
        "memmove",
        &[S(DATA), S(DATA + 0x300), V(31)],
        DATA + 0xa0,
        true,
    );
    call(
        &mut machine,
        &mut code,
        MODULE,
        "memchr",
        &[V(SOURCE), V(0x1ff), V(SIZE)],
        DATA + 0xc0,
        true,
    );
    call(
        &mut machine,
        &mut code,
        MODULE,
        "memchr",
        &[V(SOURCE), V(254), V(SIZE)],
        DATA + 0xe0,
        true,
    );
    for (index, name) in ["memset", "memcpy", "memmove", "memchr"]
        .into_iter()
        .enumerate()
    {
        call(
            &mut machine,
            &mut code,
            MODULE,
            name,
            &[V(NOACCESS), V(NOACCESS), V(0)],
            DATA + 0x100 + index as u32 * 0x20,
            true,
        );
    }
    call(
        &mut machine,
        &mut code,
        "msvcrt.dll",
        "memset",
        &[V(DATA + 0xc00), V(0xffff_ff9a), V(17)],
        DATA + 0x180,
        true,
    );
    call(
        &mut machine,
        &mut code,
        "msvcrt.dll",
        "memcpy",
        &[V(DATA + 0xc40), V(SOURCE + 100), V(53)],
        DATA + 0x1a0,
        true,
    );
    call(
        &mut machine,
        &mut code,
        "msvcrt.dll",
        "memmove",
        &[V(DATA + 0xd05), V(DATA + 0xd00), V(11)],
        DATA + 0x1c0,
        true,
    );
    code.call(
        machine
            .resolve_import("kernel32.dll", "GetLastError")
            .unwrap(),
    )
    .mov_mem_eax(DATA + 0x320);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("VCRuntime memory exports", CODE, &[], SIZE)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let allocated = machine.cpu.read_u32(DATA).unwrap();
    assert_ne!(allocated, 0);
    assert_eq!(machine.sys.vm.region(allocated), Some(SIZE));
    let expected = [
        allocated,
        allocated,
        SNAPSHOT + 4,
        allocated,
        allocated + 5,
        allocated,
        SOURCE + SIZE - 1,
        0,
        NOACCESS,
        NOACCESS,
        NOACCESS,
        0,
        DATA + 0xc00,
        DATA + 0xc40,
        DATA + 0xd05,
    ];
    for (index, pointer) in expected.into_iter().enumerate() {
        let output = DATA + index as u32 * 0x20;
        assert_eq!(
            machine.cpu.read_u32(output).unwrap(),
            pointer,
            "call {index}"
        );
        let before = machine.cpu.read_u32(output + 4).unwrap();
        assert_eq!(
            machine.cpu.read_u32(output + 8).unwrap(),
            before - if index == 0 { 0 } else { 12 }
        );
        assert_eq!(machine.cpu.read_u32(output + 12).unwrap(), before);
    }
    let mut snapshot = vec![0; SIZE as usize + 8];
    machine.cpu.read(SNAPSHOT, &mut snapshot).unwrap();
    assert_eq!(&snapshot[..4], &[0xcc; 4]);
    assert!(
        snapshot[4..SIZE as usize + 4]
            .iter()
            .all(|&byte| byte == 0x5a)
    );
    assert_eq!(&snapshot[SIZE as usize + 4..], &[0xcc; 4]);
    let mut actual = vec![0; SIZE as usize];
    machine.cpu.read(allocated, &mut actual).unwrap();
    source.copy_within(0..37, 5);
    source.copy_within(5..36, 0);
    assert_eq!(actual, source);
    let mut untouched = [0; 32];
    machine.cpu.read(NOACCESS, &mut untouched).unwrap();
    assert_eq!(untouched, [0x77; 32]);
    let mut legacy_fill = [0; 17];
    machine.cpu.read(DATA + 0xc00, &mut legacy_fill).unwrap();
    assert_eq!(legacy_fill, [0x9a; 17]);
    let mut legacy_copy = [0; 53];
    machine.cpu.read(DATA + 0xc40, &mut legacy_copy).unwrap();
    assert_eq!(legacy_copy.as_slice(), &source[100..153]);
    let mut legacy_move = [0; 32];
    machine.cpu.read(DATA + 0xd00, &mut legacy_move).unwrap();
    let mut legacy_expected: Vec<u8> = (0..32).collect();
    legacy_expected.copy_within(0..11, 5);
    assert_eq!(legacy_move.as_slice(), legacy_expected);
    assert_eq!(machine.cpu.read_u32(DATA + 0x320).unwrap(), 0x31415926);
}

#[test]
fn vcruntime_memory_exports_preserve_owners_results_and_x86_stack_cleanup() {
    memory_exports(JitCpu::new(PARK_ADDR, false).unwrap());
    memory_exports(JitCpu::new(PARK_ADDR, true).unwrap());
    memory_exports(UnicornCpu::new(PARK_ADDR).unwrap());
}
