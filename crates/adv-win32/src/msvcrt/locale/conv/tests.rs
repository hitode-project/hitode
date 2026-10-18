use super::super::tests::{call, env, fixture, machine};
use super::*;
use adv_abi::Conv;
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{Machine, RunResult, layout::PARK_ADDR};
use adv_unicorn::UnicornCpu;

const MODULE: &str = "msvcr90.dll";
const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;

fn pointer<B: Backend>(machine: &mut Machine<B>, entry: u32) -> u32 {
    match get(&mut env(machine, entry)) {
        Outcome::Return(pointer) => pointer,
        _ => panic!("supported lconv query must return its owned data"),
    }
}
fn data<B: Backend>(machine: &Machine<B>, pointer: u32) -> (Vec<Vec<u8>>, [u8; 8]) {
    let mut prefix = [0; PREFIX_BYTES];
    machine.cpu.read(pointer, &mut prefix).unwrap();
    let strings = prefix[..40]
        .as_chunks::<4>()
        .0
        .iter()
        .map(|bytes| {
            let address = u32::from_le_bytes(*bytes);
            assert!(address >= pointer + PREFIX_BYTES as u32);
            // Every pointer, including the C locale's empty strings, is non-NULL
            // and belongs to the same allocation as the lconv prefix.
            assert!(address < pointer + 128);
            AnsiStr(GuestPtr::new(address))
                .read(&machine.cpu, 32)
                .unwrap()
        })
        .collect();
    (strings, prefix[40..48].try_into().unwrap())
}
fn expected(strings: [&[u8]; 10], flags: [u8; 8]) -> (Vec<Vec<u8>>, [u8; 8]) {
    (strings.iter().map(|s| s.to_vec()).collect(), flags)
}

#[test]
fn localeconv_matches_vc90_numeric_monetary_profiles_and_mixed_categories() {
    // Independent native VC90 capture, source SHA A5091D64...C85BE986 in
    // data/abi/crt-source.json. The modern SDK wide tail is deliberately absent.
    let (mut machine, setter) = fixture();
    let entry = machine.resolve_import(MODULE, "localeconv").unwrap();
    let c = expected(
        [b".", b"", b"", b"", b"", b"", b"", b"", b"", b""],
        [127; 8],
    );
    let jp = expected(
        [
            b".", b",", b"\x03", b"JPY", b"\\", b".", b",", b"\x03", b"", b"-",
        ],
        [0, 0, 1, 0, 1, 0, 3, 3],
    );
    let cn = expected(
        [
            b".",
            b",",
            b"\x03",
            b"CNY",
            b"\xa3\xa4",
            b".",
            b",",
            b"\x03",
            b"",
            b"-",
        ],
        [2, 2, 1, 0, 1, 0, 4, 4],
    );
    let en = expected(
        [
            b".", b",", b"\x03", b"USD", b"$", b".", b",", b"\x03", b"", b"-",
        ],
        [2, 2, 1, 0, 1, 0, 3, 0],
    );
    for (locale, golden) in [
        ("C", &c),
        ("Japanese_Japan.932", &jp),
        ("Chinese (Simplified)_China.936", &cn),
        ("English_United States.1252", &en),
    ] {
        assert_ne!(call(&mut machine, setter, 0, Some(locale)), 0);
        let address = pointer(&mut machine, entry);
        assert_eq!(&data(&machine, address), golden, "{locale}");
        assert!(
            machine
                .ext
                .get::<Heaps>()
                .owner(&machine.cpu, address)
                .is_some()
        );
    }
    // C numeric with English monetary must retain empty numeric grouping, but
    // publish the localized monetary separators, flags and sign independently.
    call(&mut machine, setter, 0, Some("C"));
    call(&mut machine, setter, 3, Some("English_United States.1252"));
    let address = pointer(&mut machine, entry);
    assert_eq!(
        data(&machine, address),
        expected(
            [b".", b"", b"", b"USD", b"$", b".", b",", b"\x03", b"", b"-"],
            [2, 2, 1, 0, 1, 0, 3, 0]
        )
    );
    call(&mut machine, setter, 4, Some("Japanese_Japan.932"));
    let address = pointer(&mut machine, entry);
    assert_eq!(data(&machine, address), en);
    call(&mut machine, setter, 0, Some("Japanese_Japan.932"));
    call(&mut machine, setter, 4, Some("English_United States.1252"));
    call(
        &mut machine,
        setter,
        3,
        Some("Chinese (Simplified)_China.936"),
    );
    let address = pointer(&mut machine, entry);
    let mut mixed = cn;
    mixed.0[4] = vec![0x5c];
    assert_eq!(data(&machine, address), mixed);
    assert_eq!(machine.ext.get::<crate::Win32>().codec.acp, 932);
}

#[test]
fn localeconv_encoding_snapshots_cached_readonly_data_and_module_detach_have_one_owner() {
    let (mut machine, setter) = fixture();
    let entry = machine.resolve_import(MODULE, "localeconv").unwrap();
    let errno = floating::errno_address(&mut env(&mut machine, entry));
    errno.write(&mut machine.cpu, 79).unwrap();
    crate::util::set_last_error(&mut env(&mut machine, entry), 0x12345678);
    let initial = pointer(&mut machine, entry);
    let mut owned = vec![initial];
    call(&mut machine, setter, 0, Some("Japanese_Japan.932"));
    call(
        &mut machine,
        setter,
        3,
        Some("Chinese (Simplified)_China.936"),
    );
    let cn932 = pointer(&mut machine, entry);
    assert_eq!(data(&machine, cn932).0[4], b"\\");
    owned.push(cn932);
    let before = data(&machine, cn932);
    call(&mut machine, setter, 2, Some("English_United States.1252"));
    // This no-op monetary set also must not rebuild using the new CTYPE.
    call(
        &mut machine,
        setter,
        3,
        Some("Chinese (Simplified)_China.936"),
    );
    assert_eq!(pointer(&mut machine, entry), cn932);
    assert_eq!(data(&machine, cn932), before);
    call(
        &mut machine,
        setter,
        3,
        Some("Chinese (Simplified)_China.1252"),
    );
    let cn1252 = pointer(&mut machine, entry);
    assert_eq!(data(&machine, cn1252).0[4], b"\xa5");
    owned.push(cn1252);
    call(&mut machine, setter, 2, Some("C"));
    assert_eq!(pointer(&mut machine, entry), cn1252);
    call(
        &mut machine,
        setter,
        3,
        Some("Chinese (Simplified)_China.936"),
    );
    let cn936 = pointer(&mut machine, entry);
    assert_eq!(data(&machine, cn936).0[4], b"\xa3\xa4");
    owned.push(cn936);
    // Match the native LC_ALL unchanged-monetary case: changing CTYPE to936
    // while keeping CN.936 monetary leaves its prior932 encoding untouched.
    call(&mut machine, setter, 0, Some("Japanese_Japan.932"));
    owned.push(pointer(&mut machine, entry));
    call(
        &mut machine,
        setter,
        3,
        Some("Chinese (Simplified)_China.936"),
    );
    assert_eq!(pointer(&mut machine, entry), cn932);
    call(
        &mut machine,
        setter,
        0,
        Some("Chinese (Simplified)_China.936"),
    );
    let cn932_numeric_cn = pointer(&mut machine, entry);
    assert_eq!(data(&machine, cn932_numeric_cn).0[4], b"\\");
    owned.push(cn932_numeric_cn);
    let before = data(&machine, cn932_numeric_cn);
    machine
        .cpu
        .protect(cn932_numeric_cn & !4095, 4096, Prot::READ)
        .unwrap();
    assert_eq!(pointer(&mut machine, entry), cn932_numeric_cn);
    assert_eq!(data(&machine, cn932_numeric_cn), before);
    machine
        .cpu
        .protect(cn932_numeric_cn & !4095, 4096, Prot::RW)
        .unwrap();
    // Exercise the shared owner through an existing80 thunk without claiming
    // another localeconv export. Each actual CRT module owns separate storage.
    let other = machine.resolve_import("msvcr80.dll", "_errno").unwrap();
    let second = pointer(&mut machine, other);
    assert!(!owned.contains(&second));
    assert_eq!(data(&machine, second).1, [127; 8]);
    owned.push(second);
    assert_eq!(errno.read(&machine.cpu).unwrap(), 79);
    assert_eq!(
        crate::util::last_error(&env(&mut machine, entry)),
        0x12345678
    );
    super::super::super::detach_thread(&mut env(&mut machine, entry)).unwrap();
    for &address in &owned {
        assert!(
            machine
                .ext
                .get::<Heaps>()
                .owner(&machine.cpu, address)
                .is_some()
        );
    }
    super::super::super::detach_process(&mut env(&mut machine, entry)).unwrap();
    for address in owned {
        assert!(
            machine
                .ext
                .get::<Heaps>()
                .owner(&machine.cpu, address)
                .is_none()
        );
    }
    assert!(machine.ext.get::<CrtState>().locales.is_empty());
    super::super::super::detach_process(&mut env(&mut machine, entry)).unwrap();
}

fn guest_getter<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    let entry = machine.resolve_import(MODULE, "localeconv").unwrap();
    let copy = machine.resolve_import(MODULE, "memcpy").unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(0x12345678).call(set_error);
    code.call(entry).mov_mem_eax(DATA);
    code.mov_eax_ptr_eax()
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 4);
    code.push_imm(PREFIX_BYTES as u32);
    code.bytes.extend_from_slice(&[0xff, 0x35]); // push dword ptr [DATA]
    code.bytes.extend_from_slice(&DATA.to_le_bytes());
    code.push_imm(DATA + 64).call(copy).add_esp(12);
    code.call(get_error).mov_mem_eax(DATA + 8);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn(
            "VC90 localeconv zero-argument cdecl and copied48byte prefix",
            CODE,
            &[],
            65536,
        )
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let address = machine.cpu.read_u32(DATA).unwrap();
    assert_ne!(address, 0);
    // DWORD load from the first string is safe: '.' NUL followed by another
    // owned empty string and its NUL. This checks two levels of guest pointers.
    assert_eq!(machine.cpu.read_u32(DATA + 4).unwrap(), u32::from(b'.'));
    assert_eq!(machine.cpu.read_u32(DATA + 8).unwrap(), 0x12345678);
    let mut original = [0; PREFIX_BYTES];
    let mut copied = [0; PREFIX_BYTES];
    machine.cpu.read(address, &mut original).unwrap();
    machine.cpu.read(DATA + 64, &mut copied).unwrap();
    assert_eq!(original, copied);
    assert_eq!(&copied[40..48], &[127; 8]);
}
#[test]
fn localeconv_actual_cdecl_zero_arguments_and48byte_layout_run_in_interpreter_and_jit() {
    let mut registry = Registry::default();
    super::super::super::register(&mut registry);
    assert_eq!(
        registry.get(MODULE, "localeconv").unwrap().conv,
        Conv::Cdecl { argc: 0 }
    );
    assert!(registry.get("msvcr80.dll", "localeconv").is_none());
    assert!(registry.get("ucrtbase.dll", "localeconv").is_none());
    guest_getter(UnicornCpu::new(PARK_ADDR).unwrap());
    guest_getter(JitCpu::new(PARK_ADDR, false).unwrap());
}
