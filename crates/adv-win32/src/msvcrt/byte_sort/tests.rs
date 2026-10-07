//! VC80/90 native boundary contract: crt-sort-native/probe.c/native.txt.
//! Equal keys and callback order need not match native; caller element
//! addresses, data integrity, cdecl transport and error owners must match.
use super::*;
use adv_abi::{Conv, signature::AbiKind};
use adv_cpu::{
    JitCpu,
    backend::{Backend, GuestExt},
};
use adv_machine::{Clock, Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;
use iced_x86::code_asm::*;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const KEYS: [u8; 13] = [9, 1, 8, 2, 7, 3, 6, 4, 5, 1, 0, 9, 2];

fn fixture<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    for module in MODULES {
        let signature = adv_abi::signature::lookup(module, "qsort").unwrap();
        assert_eq!(signature.conv, Some(Conv::Cdecl { argc: 4 }));
        assert_eq!(signature.return_kind, AbiKind::Void);
        assert_eq!(signature.stack_words(), 4);
        assert!(registry.dynamic_available(module, "qsort"));
    }
    assert!(registry.signature_conflicts().is_empty());
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-sort-tests");
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&root, "fixture.exe", &root),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 16384, Prot::RW).unwrap();
    machine
}
fn write<B: Backend>(machine: &mut Machine<B>, address: u32, mut code: CodeAssembler) {
    let bytes = code.assemble(u64::from(address)).unwrap();
    machine.cpu.write(address, &bytes).unwrap();
}
fn call_sort(code: &mut CodeAssembler, entry: u32, base: u32, count: u32, width: u32, cmp: u32) {
    for argument in [cmp, width, count, base] {
        code.push(argument).unwrap();
    }
    code.call(u64::from(entry)).unwrap();
    code.add(esp, 16).unwrap();
}
fn seed<B: Backend>(machine: &mut Machine<B>, code: &mut CodeAssembler, module: &str, output: u32) {
    let errno = machine.resolve_import(module, "_errno").unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    code.call(u64::from(errno)).unwrap();
    code.mov(dword_ptr(output as i32), eax).unwrap();
    code.mov(dword_ptr(eax), 79).unwrap();
    code.mov(dword_ptr(eax + 4), 91).unwrap();
    code.push(77).unwrap();
    code.call(u64::from(set_error)).unwrap();
}
fn execute<B: Backend>(machine: &mut Machine<B>, code: CodeAssembler) -> RunResult {
    write(machine, CODE, code);
    machine
        .spawn("qsort cdecl guest callbacks", CODE, &[], 65536)
        .unwrap();
    machine.run_virtual(1_000_000)
}

fn guest<B: Backend>(cpu: impl Fn() -> B) {
    let mut machine = fixture(cpu());
    let sleep = machine.resolve_import("kernel32.dll", "Sleep").unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut main = CodeAssembler::new(32).unwrap();
    main.mov(dword_ptr((DATA + 400) as i32), esp).unwrap();
    for (index, module) in MODULES.into_iter().enumerate() {
        let array = DATA + 512 + index as u32 * 256;
        let counter = DATA + index as u32 * 128;
        let log = DATA + 4096 + index as u32 * 2048;
        let comparator = CODE + 0x800 + index as u32 * 256;
        let original: Vec<u8> = KEYS
            .into_iter()
            .enumerate()
            .flat_map(|(id, key)| [key, id as u8, 0x80 | id as u8])
            .collect();
        machine.cpu.write(array, &original).unwrap();
        let mut callback = CodeAssembler::new(32).unwrap();
        callback.mov(eax, dword_ptr(esp + 4)).unwrap();
        callback.mov(edx, dword_ptr(esp + 8)).unwrap();
        callback.mov(ecx, dword_ptr(counter as i32)).unwrap();
        callback.mov(dword_ptr(ecx * 8 + log as i32), eax).unwrap();
        callback
            .mov(dword_ptr(ecx * 8 + (log + 4) as i32), edx)
            .unwrap();
        callback.inc(dword_ptr(counter as i32)).unwrap();
        // A comparator may wait. It resumes on the same guest cdecl frame;
        // no host borrow or copied array element survives this wait.
        callback.push(eax).unwrap();
        callback.push(edx).unwrap();
        callback.push(1).unwrap();
        callback.call(u64::from(sleep)).unwrap();
        callback.pop(edx).unwrap();
        callback.pop(eax).unwrap();
        callback.movzx(eax, byte_ptr(eax)).unwrap();
        callback.movzx(edx, byte_ptr(edx)).unwrap();
        callback.sub(eax, edx).unwrap();
        callback.ret().unwrap();
        write(&mut machine, comparator, callback);
        let sort = machine.resolve_import(module, "qsort").unwrap();
        seed(&mut machine, &mut main, module, counter + 4);
        call_sort(&mut main, sort, array, 13, 3, comparator);
        main.mov(eax, dword_ptr(counter as i32)).unwrap();
        main.mov(dword_ptr((counter + 16) as i32), eax).unwrap();
        call_sort(&mut main, sort, 0, 0, 3, comparator);
        call_sort(&mut main, sort, array, 1, 3, comparator);
        main.mov(dword_ptr((counter + 20) as i32), esp).unwrap();
        main.mov(eax, dword_ptr((counter + 4) as i32)).unwrap();
        main.mov(edx, dword_ptr(eax + 4)).unwrap();
        main.mov(dword_ptr((counter + 12) as i32), edx).unwrap();
        main.mov(eax, dword_ptr(eax)).unwrap();
        main.mov(dword_ptr((counter + 8) as i32), eax).unwrap();
        main.call(u64::from(get_error)).unwrap();
        main.mov(dword_ptr((counter + 24) as i32), eax).unwrap();
    }
    // Returning invalid-parameter callback uses the existing VC80 owner.
    let handler = CODE + 0x600;
    let mut callback = CodeAssembler::new(32).unwrap();
    callback.inc(dword_ptr((DATA + 384) as i32)).unwrap();
    callback.ret().unwrap();
    write(&mut machine, handler, callback);
    let setter = machine
        .resolve_import("msvcr80.dll", "_set_invalid_parameter_handler")
        .unwrap();
    main.push(handler).unwrap();
    main.call(u64::from(setter)).unwrap();
    main.add(esp, 4).unwrap();
    let sort = machine.resolve_import("msvcr80.dll", "qsort").unwrap();
    call_sort(&mut main, sort, 0, 0, 4, 0);
    let errno = machine.resolve_import("msvcr80.dll", "_errno").unwrap();
    main.call(u64::from(errno)).unwrap();
    main.mov(eax, dword_ptr(eax)).unwrap();
    main.mov(dword_ptr((DATA + 388) as i32), eax).unwrap();
    main.call(u64::from(get_error)).unwrap();
    main.mov(dword_ptr((DATA + 392) as i32), eax).unwrap();
    main.xor(eax, eax).unwrap();
    main.ret().unwrap();
    let result = execute(&mut machine, main);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let mut expected_keys = KEYS;
    expected_keys.sort();
    for index in 0..3u32 {
        let array = DATA + 512 + index * 256;
        let counter = DATA + index * 128;
        let log = DATA + 4096 + index * 2048;
        let mut sorted = [0; 39];
        machine.cpu.read(array, &mut sorted).unwrap();
        let records: [[u8; 3]; 13] = sorted.as_chunks::<3>().0.try_into().unwrap();
        assert_eq!(records.map(|record| record[0]), expected_keys);
        let mut identities = records.map(|record| record[1]);
        identities.sort();
        assert_eq!(identities, std::array::from_fn::<_, 13, _>(|id| id as u8));
        for record in records {
            assert_eq!(record[0], KEYS[usize::from(record[1])]);
            assert_eq!(record[2], 0x80 | record[1]);
        }
        let comparisons = machine.cpu.read_u32(counter).unwrap();
        assert!(comparisons > 0 && comparisons < 256);
        assert_eq!(machine.cpu.read_u32(counter + 16).unwrap(), comparisons);
        for offset in 0..comparisons * 2 {
            let address = machine.cpu.read_u32(log + offset * 4).unwrap();
            assert!(address >= array && address < array + 39);
            assert!((address - array).is_multiple_of(3));
        }
        for (offset, expected) in [(8, 79), (12, 91), (24, 77)] {
            assert_eq!(machine.cpu.read_u32(counter + offset).unwrap(), expected);
        }
        assert_eq!(
            machine.cpu.read_u32(counter + 20).unwrap(),
            machine.cpu.read_u32(DATA + 400).unwrap()
        );
    }
    assert_eq!(machine.cpu.read_u32(DATA + 384).unwrap(), 1);
    assert_eq!(machine.cpu.read_u32(DATA + 388).unwrap(), 22);
    assert_eq!(machine.cpu.read_u32(DATA + 392).unwrap(), 0);

    // A comparator can remove write permission. No swap publishes either
    // element before both have passed a fresh complete READ|WRITE guard.
    let mut machine = fixture(cpu());
    let array = machine
        .sys
        .vm
        .alloc(&mut machine.cpu, 4096, Prot::RW)
        .unwrap();
    let original = [1, 0x11, 0xa1, 2, 0x22, 0xa2];
    machine.cpu.write(array, &original).unwrap();
    let protect = machine
        .resolve_import("kernel32.dll", "VirtualProtect")
        .unwrap();
    let comparator = CODE + 0x800;
    let mut callback = CodeAssembler::new(32).unwrap();
    callback.inc(dword_ptr((DATA + 32) as i32)).unwrap();
    callback.push(-1).unwrap(); // force a swap after the callback.
    for argument in [DATA + 16, 2, 4096, array] {
        callback.push(argument).unwrap();
    }
    callback.call(u64::from(protect)).unwrap();
    callback.pop(eax).unwrap();
    callback.ret().unwrap();
    write(&mut machine, comparator, callback);
    let sort = machine.resolve_import("msvcr90.dll", "qsort").unwrap();
    let mut main = CodeAssembler::new(32).unwrap();
    call_sort(&mut main, sort, array, 2, 3, comparator);
    main.ret().unwrap();
    let result = execute(&mut machine, main);
    assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
    let mut actual = [0; 6];
    machine.cpu.read(array, &mut actual).unwrap();
    assert_eq!(actual, original);
    assert_eq!(machine.cpu.read_u32(DATA + 32).unwrap(), 1);

    // Range overflow/limit and legacy invalid profiles are explicit failures;
    // initial read-only arrays fail before a callback or byte change.
    for (module, base, count, width, compare, read_only, warning) in [
        ("msvcr90.dll", DATA + 8192, 2, 3, CODE + 0x800, true, false),
        (
            "msvcr90.dll",
            DATA + 8192,
            MAX_COUNT + 1,
            1,
            CODE + 0x800,
            false,
            true,
        ),
        (
            "msvcr90.dll",
            DATA + 8192,
            2,
            u32::MAX,
            CODE + 0x800,
            false,
            true,
        ),
        ("msvcr90.dll", 0xfffffff8, 2, 8, CODE + 0x800, false, true),
        ("msvcrt.dll", 0, 0, 0, 0, false, true),
    ] {
        let mut machine = fixture(cpu());
        machine.cpu.write(DATA + 8192, &original).unwrap();
        if read_only {
            machine.cpu.protect(DATA + 8192, 4096, Prot::READ).unwrap();
        }
        let entry = machine.resolve_import(module, "qsort").unwrap();
        let mut main = CodeAssembler::new(32).unwrap();
        seed(&mut machine, &mut main, module, DATA + 4);
        call_sort(&mut main, entry, base, count, width, compare);
        main.ret().unwrap();
        let result = execute(&mut machine, main);
        assert!(matches!(result, RunResult::Failed(_)), "{result:?}");
        let mut actual = [0; 6];
        machine.cpu.read(DATA + 8192, &mut actual).unwrap();
        assert_eq!(actual, original);
        let errno = machine.cpu.read_u32(DATA + 4).unwrap();
        assert_eq!(machine.cpu.read_u32(errno).unwrap(), 79);
        assert_eq!(machine.cpu.read_u32(errno + 4).unwrap(), 91);
        if warning {
            let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
            assert_eq!(diagnostics.warnings.len(), 1);
            assert!(
                diagnostics
                    .warnings
                    .keys()
                    .any(|(api, _)| api == &format!("{module}!qsort"))
            );
        }
    }
}

#[test]
fn qsort_real_cdecl_callbacks_keep_guest_element_addresses_and_checked_swap_lifetimes() {
    guest(|| UnicornCpu::new(PARK_ADDR).unwrap());
    guest(|| JitCpu::new(PARK_ADDR, false).unwrap());
    guest(|| JitCpu::new(PARK_ADDR, true).unwrap());
}
