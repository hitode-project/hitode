use super::*;
use adv_abi::{Conv, GuestPtr};
use adv_cpu::{
    JitCpu,
    backend::{Backend, Guest, GuestExt, Prot, asm::Asm},
};
use adv_machine::{
    Clock, Machine, RunResult,
    layout::{PARK_ADDR, THUNK_BASE, THUNK_STRIDE},
};
use adv_unicorn::UnicornCpu;

const CODE: u32 = 0x400000;
const DATA: u32 = 0x410000;
const REQUEST: u32 = DATA + 1024;
const CALLBACK: u32 = CODE + 512;
const MODULE: &str = "msvcr90.dll";

pub(super) fn machine<B: Backend>(cpu: B) -> Machine<B> {
    let mut registry = Registry::default();
    crate::register(&mut registry);
    super::super::register(&mut registry);
    assert!(registry.signature_conflicts().is_empty());
    assert_eq!(
        registry.get(MODULE, "setlocale").unwrap().conv,
        Conv::Cdecl { argc: 2 }
    );
    let mut machine = Machine::new(cpu, registry, Clock::virtual_time()).unwrap();
    let directory =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/crt-locale-tests");
    crate::Win32::install(
        &mut machine,
        crate::Win32Config::new(&directory, "fixture.exe", &directory),
    )
    .unwrap();
    machine.cpu.map(CODE, 4096, Prot::RX).unwrap();
    machine.cpu.map(DATA, 4096, Prot::RW).unwrap();
    machine
}
pub(super) fn fixture() -> (Machine<JitCpu>, u32) {
    let mut machine = machine(JitCpu::new(PARK_ADDR, false).unwrap());
    machine
        .spawn("locale owner", PARK_ADDR, &[], 65536)
        .unwrap();
    let entry = machine.resolve_import(MODULE, "setlocale").unwrap();
    (machine, entry)
}
pub(super) fn env<B: Backend>(machine: &mut Machine<B>, entry: u32) -> Env<'_> {
    Env {
        context: machine.sys.current,
        cpu: &mut machine.cpu,
        sys: &mut machine.sys,
        ext: &mut machine.ext,
        thunk: (entry - THUNK_BASE) / THUNK_STRIDE,
    }
}
pub(super) fn call<B: Backend>(
    machine: &mut Machine<B>,
    entry: u32,
    category: i32,
    request: Option<&str>,
) -> u32 {
    let address = if let Some(request) = request {
        let mut bytes = request.as_bytes().to_vec();
        bytes.push(0);
        machine.cpu.write(REQUEST, &bytes).unwrap();
        REQUEST
    } else {
        0
    };
    match setlocale(
        &mut env(machine, entry),
        category,
        AnsiStr(GuestPtr::new(address)),
    ) {
        Outcome::Return(value) => value,
        _ => panic!("valid query/set should return immediately"),
    }
}
fn string<B: Backend>(machine: &Machine<B>, pointer: u32) -> String {
    let bytes = AnsiStr(GuestPtr::new(pointer))
        .read(&machine.cpu, BUFFER_BYTES as usize)
        .unwrap();
    String::from_utf8(bytes).unwrap()
}

#[test]
fn initial_queries_c_save_restore_and_crt_module_owners_have_real_process_lifetime() {
    let (mut machine, entry) = fixture();
    let pointer = call(&mut machine, entry, 0, None);
    assert_ne!(pointer, 0);
    assert_eq!(string(&machine, pointer), "C");
    assert!(
        machine
            .ext
            .get::<Heaps>()
            .owner(&machine.cpu, pointer)
            .is_some()
    );
    let saved = string(&machine, pointer);
    for category in 0..=5 {
        assert_eq!(call(&mut machine, entry, category, None), pointer);
        assert_eq!(string(&machine, pointer), "C");
    }
    assert_eq!(call(&mut machine, entry, 0, Some("Japanese")), pointer);
    assert_eq!(string(&machine, pointer), "Japanese_Japan.932");
    assert_eq!(call(&mut machine, entry, 0, Some(&saved)), pointer);
    assert_eq!(string(&machine, pointer), "C");

    // Exercise the shared owner helper through an existing second CRT thunk,
    // without registering an extra setlocale export in that module.
    let other = machine.resolve_import("msvcr80.dll", "_errno").unwrap();
    let second = call(&mut machine, other, 0, None);
    assert_ne!(second, pointer);
    assert_eq!(call(&mut machine, entry, 2, Some(".936")), pointer);
    let queried = call(&mut machine, other, 0, None);
    assert_eq!(string(&machine, queried), "C");
    let queried = call(&mut machine, entry, 0, None);
    let mixed = string(&machine, queried);
    assert_eq!(
        mixed,
        "LC_COLLATE=C;LC_CTYPE=Japanese_Japan.936;LC_MONETARY=C;LC_NUMERIC=C;LC_TIME=C"
    );
    call(&mut machine, entry, 0, Some("C"));
    call(&mut machine, entry, 0, Some(&mixed));
    let queried = call(&mut machine, entry, 0, None);
    assert_eq!(string(&machine, queried), mixed);
    assert_eq!(machine.ext.get::<CrtState>().locales.len(), 2);
    super::super::detach_thread(&mut env(&mut machine, entry)).unwrap();
    assert!(
        machine
            .ext
            .get::<Heaps>()
            .owner(&machine.cpu, pointer)
            .is_some()
    );
    super::super::detach_process(&mut env(&mut machine, entry)).unwrap();
    assert!(
        machine
            .ext
            .get::<Heaps>()
            .owner(&machine.cpu, pointer)
            .is_none()
    );
    assert!(
        machine
            .ext
            .get::<Heaps>()
            .owner(&machine.cpu, second)
            .is_none()
    );
    assert!(machine.ext.get::<CrtState>().locales.is_empty());
    super::super::detach_process(&mut env(&mut machine, entry)).unwrap();
}

#[test]
fn guest_defaults_invalid_locale_and_bad_memory_keep_encoding_errno_and_prior_state() {
    let (mut machine, entry) = fixture();
    for (acp, expected) in [
        (932, "Japanese_Japan.932"),
        (936, "Chinese (Simplified)_China.936"),
    ] {
        machine.ext.get_mut::<crate::Win32>().codec = adv_os::Codec::new(acp);
        let pointer = call(&mut machine, entry, 0, Some(""));
        assert_eq!(string(&machine, pointer), expected);
        assert_eq!(machine.ext.get::<crate::Win32>().codec.acp, acp);
        let errno = floating::errno_address(&mut env(&mut machine, entry));
        errno.write(&mut machine.cpu, 79).unwrap();
        crate::util::set_last_error(&mut env(&mut machine, entry), 0x12345678);
        for request in [
            "c",
            "not-a-locale",
            ".UTF8",
            "LC_CTYPE=C;LC_TIME=unsupported",
        ] {
            assert_eq!(call(&mut machine, entry, 0, Some(request)), 0);
            assert_eq!(string(&machine, pointer), expected);
            assert_eq!(errno.read(&machine.cpu).unwrap(), 79);
            assert_eq!(
                crate::util::last_error(&env(&mut machine, entry)),
                0x12345678
            );
        }
        let Outcome::Fail(_) = setlocale(
            &mut env(&mut machine, entry),
            0,
            AnsiStr(GuestPtr::new(0xdead0000)),
        ) else {
            panic!("unmapped locale pointer accepted")
        };
        assert_eq!(string(&machine, pointer), expected);
        let before = machine.ext.get::<CrtState>().locales[MODULE].categories;
        machine
            .cpu
            .protect(pointer & !4095, 4096, Prot::READ)
            .unwrap();
        machine.cpu.write(REQUEST, b"C\0").unwrap();
        let Outcome::Fail(_) = setlocale(
            &mut env(&mut machine, entry),
            0,
            AnsiStr(GuestPtr::new(REQUEST)),
        ) else {
            panic!("read-only returned buffer accepted")
        };
        assert!(machine.ext.get::<CrtState>().locales[MODULE].categories == before);
        assert_eq!(string(&machine, pointer), expected);
        machine
            .cpu
            .protect(pointer & !4095, 4096, Prot::RW)
            .unwrap();
    }
    let diagnostics = machine.ext.get::<adv_abi::ApiDiagnostics>();
    assert_eq!(diagnostics.warnings.len(), 1);
    assert_eq!(*diagnostics.warnings.values().next().unwrap(), 8);
}

fn guest_roundtrip<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    let locale = machine.resolve_import(MODULE, "setlocale").unwrap();
    let errno = machine.resolve_import(MODULE, "_errno").unwrap();
    let setter = machine
        .resolve_import(MODULE, "_set_invalid_parameter_handler")
        .unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let get_acp = machine.resolve_import("kernel32.dll", "GetACP").unwrap();
    machine.cpu.write(REQUEST, b"C\0").unwrap();
    let mut callback = Asm::new(CALLBACK);
    callback.bytes.extend_from_slice(&[0xff, 0x05]); // inc [callback_count]
    callback.bytes.extend_from_slice(&(DATA + 32).to_le_bytes());
    callback.ret();
    machine.cpu.write(CALLBACK, &callback.bytes).unwrap();
    let mut code = Asm::new(CODE);
    code.call(errno).mov_mem_eax(DATA);
    code.push_imm(0x12345678).call(set_error);
    // Actual failing game path: LC_ALL (0), NULL, x86 cdecl caller cleanup.
    code.push_imm(0)
        .push_imm(0)
        .call(locale)
        .add_esp(8)
        .mov_mem_eax(DATA + 4);
    code.mov_eax_ptr_eax().mov_mem_eax(DATA + 28);
    code.push_imm(REQUEST)
        .push_imm(0)
        .call(locale)
        .add_esp(8)
        .mov_mem_eax(DATA + 8);
    code.push_imm(CALLBACK).call(setter).add_esp(4);
    code.push_imm(0)
        .push_imm(6)
        .call(locale)
        .add_esp(8)
        .mov_mem_eax(DATA + 12);
    code.mov_eax_mem(DATA)
        .mov_eax_ptr_eax()
        .mov_mem_eax(DATA + 16);
    code.call(get_error).mov_mem_eax(DATA + 20);
    code.call(get_acp).mov_mem_eax(DATA + 24);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("setlocale guest cdecl", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    let pointer = machine.cpu.read_u32(DATA + 4).unwrap();
    assert_ne!(pointer, 0);
    assert_eq!(machine.cpu.read_u32(DATA + 28).unwrap(), u32::from(b'C'));
    assert_eq!(machine.cpu.read_u32(DATA + 8).unwrap(), pointer);
    assert_eq!(machine.cpu.read_u32(DATA + 12).unwrap(), 0);
    assert_eq!(machine.cpu.read_u32(DATA + 16).unwrap(), 22);
    assert_eq!(machine.cpu.read_u32(DATA + 20).unwrap(), 0x12345678);
    assert_eq!(machine.cpu.read_u32(DATA + 24).unwrap(), 932);
    assert_eq!(machine.cpu.read_u32(DATA + 32).unwrap(), 1);
    assert!(
        machine
            .ext
            .try_get::<adv_abi::ApiDiagnostics>()
            .is_none_or(|d| d.warnings.is_empty())
    );
}
#[test]
fn actual_cdecl_query_and_invalid_category_callback_run_in_interpreter_and_jit() {
    guest_roundtrip(UnicornCpu::new(PARK_ADDR).unwrap());
    guest_roundtrip(JitCpu::new(PARK_ADDR, false).unwrap());
}

fn handles<B: Backend>(machine: &mut Machine<B>, entry: u32) -> u32 {
    match lc_handles(&mut env(machine, entry)) {
        Outcome::Return(pointer) => pointer,
        _ => panic!("LCID getter must return its owned array"),
    }
}
fn ids<B: Backend>(machine: &Machine<B>, pointer: u32) -> [u32; 6] {
    let mut bytes = [0; 24];
    machine.cpu.read(pointer, &mut bytes).unwrap();
    core::array::from_fn(|index| {
        u32::from_le_bytes(bytes[index * 4..index * 4 + 4].try_into().unwrap())
    })
}

#[test]
fn getter_arrays_follow_real_categories_codepage_module_owner_and_atomic_set_publication() {
    let (mut machine, set_entry) = fixture();
    let entry = machine.resolve_import(MODULE, "___lc_handle_func").unwrap();
    let cp_entry = machine
        .resolve_import(MODULE, "___lc_codepage_func")
        .unwrap();
    assert_eq!(lc_codepage(&mut env(&mut machine, cp_entry)), 0);
    let pointer = handles(&mut machine, entry);
    assert_ne!(pointer, 0);
    let allocation = pointer - LCID_OFFSET;
    assert!(
        machine
            .ext
            .get::<Heaps>()
            .owner(&machine.cpu, allocation)
            .is_some()
    );
    assert_eq!(ids(&machine, pointer), [0; 6]);
    let errno = floating::errno_address(&mut env(&mut machine, entry));
    errno.write(&mut machine.cpu, 79).unwrap();
    crate::util::set_last_error(&mut env(&mut machine, entry), 0x12345678);
    for (requested, expected, cp) in [
        ("C", [0; 6], 0),
        ("Japanese", [0, 0x411, 0x411, 0x411, 0x411, 0x411], 932),
        (
            "Chinese (Simplified)",
            [0, 0x804, 0x804, 0x804, 0x804, 0x804],
            936,
        ),
        ("English", [0, 0x409, 0x409, 0x409, 0x409, 0x409], 1252),
        (
            "LC_COLLATE=Japanese;LC_CTYPE=Chinese (Simplified);LC_TIME=English",
            [0, 0x411, 0x804, 0x409, 0x409, 0x409],
            936,
        ),
        ("C", [0; 6], 0),
    ] {
        assert_eq!(
            call(&mut machine, set_entry, 0, Some(requested)),
            allocation
        );
        // A previously returned array observes successful category changes.
        assert_eq!(ids(&machine, pointer), expected);
        assert_eq!(handles(&mut machine, entry), pointer);
        assert_eq!(lc_codepage(&mut env(&mut machine, cp_entry)), cp);
        assert_eq!(errno.read(&machine.cpu).unwrap(), 79);
        assert_eq!(
            crate::util::last_error(&env(&mut machine, entry)),
            0x12345678
        );
        assert_eq!(machine.ext.get::<crate::Win32>().codec.acp, 932);
    }
    let other = machine.resolve_import("msvcr80.dll", "_errno").unwrap();
    let other_pointer = handles(&mut machine, other);
    assert_ne!(other_pointer, pointer);
    call(&mut machine, set_entry, 2, Some("Chinese (Simplified)"));
    assert_eq!(ids(&machine, pointer), [0, 0, 0x804, 0, 0, 0]);
    assert_eq!(ids(&machine, other_pointer), [0; 6]);
    assert_eq!(lc_codepage(&mut env(&mut machine, other)), 0);
    assert_eq!(call(&mut machine, set_entry, 0, Some("unsupported")), 0);
    assert_eq!(ids(&machine, pointer), [0, 0, 0x804, 0, 0, 0]);
    machine
        .cpu
        .protect(allocation & !4095, 4096, Prot::READ)
        .unwrap();
    assert_eq!(handles(&mut machine, entry), pointer);
    assert_eq!(lc_codepage(&mut env(&mut machine, cp_entry)), 936);
    machine.cpu.write(REQUEST, b"C\0").unwrap();
    let Outcome::Fail(_) = setlocale(
        &mut env(&mut machine, set_entry),
        0,
        AnsiStr(GuestPtr::new(REQUEST)),
    ) else {
        panic!("set overwrote protected locale data")
    };
    assert_eq!(ids(&machine, pointer), [0, 0, 0x804, 0, 0, 0]);
    assert_eq!(lc_codepage(&mut env(&mut machine, cp_entry)), 936);
    machine
        .cpu
        .protect(allocation & !4095, 4096, Prot::RW)
        .unwrap();
    super::super::detach_process(&mut env(&mut machine, entry)).unwrap();
    assert!(
        machine
            .ext
            .get::<Heaps>()
            .owner(&machine.cpu, allocation)
            .is_none()
    );
    assert!(
        machine
            .ext
            .get::<Heaps>()
            .owner(&machine.cpu, other_pointer - LCID_OFFSET)
            .is_none()
    );
}

fn guest_getters<B: Backend>(cpu: B) {
    let mut machine = machine(cpu);
    let handle = machine.resolve_import(MODULE, "___lc_handle_func").unwrap();
    let cp = machine
        .resolve_import(MODULE, "___lc_codepage_func")
        .unwrap();
    let locale = machine.resolve_import(MODULE, "setlocale").unwrap();
    let set_error = machine
        .resolve_import("kernel32.dll", "SetLastError")
        .unwrap();
    let get_error = machine
        .resolve_import("kernel32.dll", "GetLastError")
        .unwrap();
    let get_acp = machine.resolve_import("kernel32.dll", "GetACP").unwrap();
    machine
        .cpu
        .write(REQUEST, b"Chinese (Simplified)\0")
        .unwrap();
    let mut code = Asm::new(CODE);
    code.push_imm(0x12345678).call(set_error);
    code.call(cp).mov_mem_eax(DATA + 4);
    code.call(handle).mov_mem_eax(DATA);
    code.mov_eax_ptr_eax().mov_mem_eax(DATA + 8);
    code.mov_eax_mem(DATA);
    code.bytes.extend_from_slice(&[0x8b, 0x40, 0x04]); // Actual caller: mov eax,[eax+4].
    code.mov_mem_eax(DATA + 12);
    code.push_imm(REQUEST).push_imm(2).call(locale).add_esp(8);
    code.mov_eax_mem(DATA);
    code.bytes.extend_from_slice(&[0x8b, 0x40, 0x08]); // Previously published LC_CTYPE.
    code.mov_mem_eax(DATA + 16);
    code.call(cp).mov_mem_eax(DATA + 20);
    code.call(get_acp).mov_mem_eax(DATA + 24);
    code.call(get_error).mov_mem_eax(DATA + 28);
    code.bytes.extend_from_slice(&[0x31, 0xc0]);
    code.ret();
    machine.cpu.write(CODE, &code.bytes).unwrap();
    machine
        .spawn("CRT locale zero-argument getters", CODE, &[], 65536)
        .unwrap();
    let result = machine.run_virtual(1_000_000);
    assert!(matches!(result, RunResult::Exited(0)), "{result:?}");
    assert_ne!(machine.cpu.read_u32(DATA).unwrap(), 0);
    for offset in [4, 8, 12] {
        assert_eq!(machine.cpu.read_u32(DATA + offset).unwrap(), 0);
    }
    assert_eq!(machine.cpu.read_u32(DATA + 16).unwrap(), 0x804);
    assert_eq!(machine.cpu.read_u32(DATA + 20).unwrap(), 936);
    assert_eq!(machine.cpu.read_u32(DATA + 24).unwrap(), 932);
    assert_eq!(machine.cpu.read_u32(DATA + 28).unwrap(), 0x12345678);
}
#[test]
fn getter_actual_zero_argument_cdecl_array_reads_run_in_interpreter_and_jit() {
    guest_getters(UnicornCpu::new(PARK_ADDR).unwrap());
    guest_getters(JitCpu::new(PARK_ADDR, false).unwrap());
}
